import SwiftUI

/// Modes, as a settings tab body: the list on the left, the editor on the
/// right, and the two sheets that can open over them.
///
/// Cross-feature destinations arrive as closures so this compiles and runs
/// without the rest of the settings shell.
struct ModesView: View {
    let store: ModesStore
    /// The global vocabulary tab. A mode's own pairs live in its editor;
    /// the list everything shares does not.
    var openVocabulary: () -> Void = {}
    /// Privacy, which owns the context ceiling and browser URL capture.
    var openPrivacy: () -> Void = {}
    /// Providers, which owns API keys and the app-wide rewrite provider.
    var openProviders: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ErrorNote(store.error)
            content
        }
        .task { await store.start() }
        .sheet(item: Binding(get: { store.pendingDelete }, set: { store.pendingDelete = $0 })) { mode in
            ModeDeleteSheet(store: store, mode: mode)
        }
        .sheet(item: Binding(get: { store.pendingConsent }, set: { _ in store.cancelConsent() })) { provider in
            ModeConsentSheet(store: store, provider: provider)
        }
    }

    @ViewBuilder private var content: some View {
        if !store.loaded {
            Text("Loading modes…").metaText()
        } else if store.modes.isEmpty {
            ModeEmptyCard(store: store)
        } else {
            HStack(alignment: .top, spacing: 24) {
                ModesList(store: store)
                    .frame(width: 320)
                ModeEditorView(
                    store: store,
                    openVocabulary: openVocabulary,
                    openPrivacy: openPrivacy,
                    openProviders: openProviders)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

/// The list on its own: usable anywhere a mode has to be picked.
struct ModesList: View {
    let store: ModesStore
    @State private var dropTarget: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Your modes").sectionLabel()
                Spacer()
                Button("New mode") { store.createMode() }
                    .buttonStyle(.secondary)
                    .disabled(store.busy)
            }
            Card {
                ForEach(Array(store.modes.enumerated()), id: \.element.id) { index, mode in
                    ModeRow(store: store, mode: mode, index: index, dropTarget: $dropTarget)
                }
            }
            if let warning = store.bindingWarning {
                Text(warning).metaText()
            }
        }
    }
}

/// One mode in the list: its name, whether it is a preset, whether it is the
/// one in force, the engine it runs on, its dictation chord, and everything
/// that can be done to it.
struct ModeRow: View {
    let store: ModesStore
    let mode: Mode
    let index: Int
    @Binding var dropTarget: String?

    private var isActive: Bool { mode.id == store.activeModeId }
    private var isSelected: Bool { store.editing?.id == mode.id }

    var body: some View {
        CardRow(action: { store.select(mode) }) {
            VStack(alignment: .leading, spacing: 2) {
                Text(mode.name)
                    .bodyText(14, isActive ? Theme.ink : Theme.inkSecondary)
                    .lineLimit(1)
                if mode.isPreset || isActive {
                    HStack(spacing: 8) {
                        if mode.isPreset {
                            Text("Preset").metaText()
                        }
                        if isActive {
                            Text("Active").font(TypeScale.label(12)).foregroundStyle(Theme.accent)
                        }
                    }
                    .fixedSize()
                }
            }
        } trailing: {
            HStack(spacing: 10) {
                Text(mode.asr.requestedEngine.summary).metaText().fixedSize()
                ModeChord(mode.shortcuts.transcribe.currentBinding)
                ModeRowMenu(store: store, mode: mode)
            }
        }
        .background(isSelected ? Theme.selection : .clear)
        .overlay(alignment: .top) {
            if dropTarget == mode.id {
                Rectangle().fill(Theme.accent).frame(height: 2)
            }
        }
        .draggable(mode.id)
        .dropDestination(for: String.self) { items, _ in
            dropTarget = nil
            guard let dragged = items.first, dragged != mode.id else { return false }
            store.move(store.modes.first { $0.id == dragged } ?? mode, to: index)
            return true
        } isTargeted: { targeted in
            dropTarget = targeted ? mode.id : nil
        }
        .onMoveCommand { direction in
            guard isSelected else { return }
            switch direction {
            case .up: store.move(mode, by: -1)
            case .down: store.move(mode, by: 1)
            default: break
            }
        }
    }
}

/// Everything a row can do, with the reasons it cannot already applied.
struct ModeRowMenu: View {
    let store: ModesStore
    let mode: Mode

    var body: some View {
        Menu {
            ForEach(store.actions(for: mode)) { action in
                Button(action.label, role: action.destructive ? .destructive : nil) {
                    run(action.kind)
                }
                .disabled(action.disabled)
            }
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Theme.inkTertiary)
                .frame(width: 24, height: 24)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("Actions for \(mode.name)")
    }

    private func run(_ kind: ModeRowAction.Kind) {
        switch kind {
        case .activate: store.activate(mode)
        case .duplicate: store.duplicate(mode)
        case .moveUp: store.move(mode, by: -1)
        case .moveDown: store.move(mode, by: 1)
        case .delete: store.pendingDelete = mode
        }
    }
}

/// Sona always keeps one mode, so an empty list means the fetch failed.
struct ModeEmptyCard: View {
    let store: ModesStore

    var body: some View {
        Card {
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text(store.error == nil ? "No modes are configured." : "Modes could not be loaded.")
                        .bodyText()
                    Text("Sona always keeps one mode. Reload to fetch the current list.").metaText()
                }
            } trailing: {
                Button("Retry") { store.reload() }.buttonStyle(.secondary)
            }
        }
    }
}

// MARK: - The editor

struct ModeEditorView: View {
    let store: ModesStore
    var openVocabulary: () -> Void = {}
    var openPrivacy: () -> Void = {}
    var openProviders: () -> Void = {}

    var body: some View {
        if let mode = store.editing {
            VStack(alignment: .leading, spacing: 0) {
                ModeEditorHeader(store: store)
                ModeEditorNotices(store: store)
                ModeInstructionsSection(store: store, mode: mode)
                ModeModelSection(store: store, mode: mode)
                ModeOutputSection(store: store, mode: mode)
                ModeActivationSection(store: store, mode: mode, openPrivacy: openPrivacy)
                ModeAdvancedSection(
                    store: store, mode: mode,
                    openVocabulary: openVocabulary,
                    openPrivacy: openPrivacy,
                    openProviders: openProviders)
            }
        } else {
            Text("Select a mode to edit it.").metaText()
        }
    }
}

struct ModeEditorHeader: View {
    let store: ModesStore

    var body: some View {
        HStack(spacing: 12) {
            InputField(prompt: "Untitled mode", text: store.field(\.name, ""))
                .frame(maxWidth: .infinity)
            Button(store.busy ? "Saving…" : "Save changes") { store.save() }
                .buttonStyle(.primary)
                .disabled(!store.canSave)
                .help("Changes apply to your next dictation.")
        }
        .padding(.bottom, 12)
    }
}

struct ModeEditorNotices: View {
    let store: ModesStore

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if store.conflict {
                ModeNotice("Settings changed elsewhere. Review the latest mode and save again.", tone: .danger)
            }
            if let blocking = store.blockingReason {
                ModeNotice(blocking, tone: .danger)
            } else if store.dirty {
                ModeNotice("Unsaved changes")
            }
        }
        .padding(.bottom, store.conflict || store.blockingReason != nil || store.dirty ? 16 : 0)
    }
}

struct ModeInstructionsSection: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        PageSection("Instructions") {
            Card {
                ToggleRow(title: "Clean up with AI", isOn: store.field(\.llm.enabled, false))
                ModeFieldRow(
                    label: "Your own instructions",
                    hint: "Anything written here replaces the output style below, rather than being added to it. Leave it empty to use the style.",
                    disabled: !mode.llm.enabled
                ) {
                    ModeTextBox(
                        text: Binding(
                            get: { mode.prompt.customPrompt ?? "" },
                            set: { value in store.edit { $0.prompt.customPrompt = value.isEmpty ? nil : value } }),
                        prompt: "Write it the way I would: short sentences, no bullet points.",
                        lines: 4)
                    .disabled(!mode.llm.enabled)
                }
                if !mode.llm.enabled {
                    ModeInlineNotice("Turn on cleanup and Sona follows these instructions after every dictation.")
                }
            }
        }
    }
}

struct ModeModelSection: View {
    let store: ModesStore
    let mode: ModeDefinition

    /// Resolved once: the label closure runs per item, and rebuilding the
    /// list inside it would rescan every model for every row.
    private var options: [ModeModelOption] { store.modelOptions(for: mode.asr.modelId) }

    var body: some View {
        PageSection("Model") {
            Card {
                ModeEngineRow(store: store, mode: mode)
                if mode.asr.requestedEngine.cloud == nil {
                    let named = options
                    ChoiceRow(
                        title: "Transcription model",
                        choices: [ModeModelOption.inherit] + named.map(\.id),
                        label: { id in
                            id == ModeModelOption.inherit
                                ? store.inheritedModelLabel
                                : named.first { $0.id == id }?.label ?? id
                        },
                        selection: Binding(
                            get: { mode.asr.modelId.isEmpty ? ModeModelOption.inherit : mode.asr.modelId },
                            set: { value in
                                store.edit { $0.asr.modelId = value == ModeModelOption.inherit ? "" : value }
                            }))
                }
                ChoiceRow(
                    title: "Language",
                    detail: mode.asr.requestedEngine.cloud == nil
                        ? nil
                        : "Choose the language sent with this cloud request.",
                    choices: ModeLanguage.all.map(\.code),
                    label: { ModeLanguage.name($0) },
                    selection: store.field(\.asr.language, "auto"))
                if let provider = mode.asr.requestedEngine.cloud, !store.cloudControlsAvailable {
                    ModeInlineNotice(store.cloudSetupNote(provider), tone: .warning)
                }
            }
        }
    }
}

/// Local first, then the cloud routes under their own heading. An engine
/// with no key in the keyring cannot be picked at all, because there is
/// nothing to consent to yet.
struct ModeEngineRow: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        CardRow {
            Text("Transcription engine").bodyText()
        } trailing: {
            Menu {
                Button("Local") { store.chooseEngine(.local) }
                Section("Cloud") {
                    ForEach(ModeCloudProvider.all) { provider in
                        Button(label(provider)) { store.chooseEngine(provider.engine) }
                            .disabled(!store.hasKey(provider))
                    }
                }
            } label: {
                Text(selectedLabel).bodyText(14, Theme.ink)
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
    }

    private var selectedLabel: String {
        mode.asr.requestedEngine.cloud?.label ?? "Local"
    }

    private func label(_ provider: ModeCloudProvider) -> String {
        store.hasKey(provider) ? provider.label : "\(provider.label) is unavailable"
    }
}

struct ModeOutputSection: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        PageSection("Output") {
            Card {
                ChoiceRow(
                    title: "Delivery method",
                    choices: ModePasteMethod.allCases,
                    label: \.label,
                    selection: store.field(\.delivery.pasteMethod, .ctrlV))
                // The script is the method's own parameter, so it stays
                // beside it: a method that cannot run without a path must
                // not hide the path.
                if mode.delivery.pasteMethod == .externalScript {
                    ModeFieldRow(label: "External script") {
                        InputField(
                            prompt: "/usr/local/bin/deliver",
                            text: Binding(
                                get: { mode.delivery.externalScriptPath ?? "" },
                                set: { value in
                                    store.edit { $0.delivery.externalScriptPath = value.isEmpty ? nil : value }
                                }))
                    }
                }
                ChoiceRow(
                    title: "Output style",
                    detail: "Choose behavior by name. Preset instructions are not shown or exported.",
                    choices: ModePromptPreset.allCases,
                    label: \.label,
                    selection: store.field(\.prompt.preset, .generic))
                .disabled(!mode.llm.enabled)
            }
        }
    }
}

// MARK: - Activation

struct ModeActivationSection: View {
    let store: ModesStore
    let mode: ModeDefinition
    var openPrivacy: () -> Void = {}
    @State private var scope: ModeWebsiteHostMatch = .suffix

    var body: some View {
        PageSection("Turns on by itself") {
            Card {
                ForEach(store.activationItems(mode.id)) { item in
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(item.target).bodyText(14)
                            if let detail = item.detail {
                                Text(detail).metaText()
                            }
                        }
                    } trailing: {
                        Button("Remove") { remove(item) }
                            .buttonStyle(.quiet)
                            .disabled(store.busy)
                            .accessibilityLabel("Remove \(item.target)")
                    }
                }
                if store.activationItems(mode.id).isEmpty {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("Nothing switches to this mode on its own.").bodyText(14)
                            Text("Each rule uses one app ID or website, such as com.apple.mail or mail.google.com.")
                                .metaText()
                        }
                    }
                }
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Apps and websites").bodyText()
                        Text("Use the website open in front. Sona saves its site name, not the full address.")
                            .metaText()
                    }
                } trailing: {
                    VStack(alignment: .trailing, spacing: 8) {
                        Button(store.busy ? "Capturing…" : "Capture current app") {
                            store.captureApp(for: mode.id)
                        }
                        .buttonStyle(.secondary)
                        .disabled(store.busy)
                        if store.websiteCaptureAllowed {
                            Picker("", selection: $scope) {
                                ForEach(ModeWebsiteHostMatch.allCases) { match in
                                    Text(match.label).tag(match)
                                }
                            }
                            .labelsHidden()
                            .pickerStyle(.menu)
                            .fixedSize()
                            Button(store.busy ? "Capturing…" : "Capture current website") {
                                store.captureWebsite(for: mode.id, match: scope)
                            }
                            .buttonStyle(.secondary)
                            .disabled(store.busy)
                        }
                    }
                }
                if !store.websiteCaptureAllowed {
                    ModeLinkNotice(
                        text: "Turn on \"Include browser URLs\" in Privacy before adding a website rule.",
                        action: "Open Privacy",
                        perform: openPrivacy)
                }
            }
        }
    }

    private func remove(_ item: ModeActivationItem) {
        if item.id.hasPrefix("app:") {
            store.removeAppRule(item.target)
        } else if item.id.hasPrefix("site:exact:") {
            store.removeWebsiteRule(item.target, match: .exact)
        } else {
            store.removeWebsiteRule(item.target, match: .suffix)
        }
    }
}

// MARK: - Advanced

/// Everything that is real but rarely touched, behind one disclosure:
/// shortcuts, tone, context, delivery, and the mode's own vocabulary.
struct ModeAdvancedSection: View {
    let store: ModesStore
    let mode: ModeDefinition
    var openVocabulary: () -> Void = {}
    var openPrivacy: () -> Void = {}
    var openProviders: () -> Void = {}
    @State private var expanded = false

    var body: some View {
        PageSection("Advanced") {
            Card {
                CardRow(action: { expanded.toggle() }) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Advanced").bodyText()
                        Text("Shortcuts, tone, context, delivery").metaText()
                    }
                } trailing: {
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.inkTertiary)
                }
                if expanded {
                    ModeShortcutsBlock(store: store, mode: mode)
                    ModeRewriteBlock(store: store, mode: mode, openProviders: openProviders)
                    ModeContextBlock(store: store, mode: mode, openPrivacy: openPrivacy)
                    ModeRecognitionBlock(store: store, mode: mode)
                    ModeDeliveryBlock(store: store, mode: mode)
                    if store.cloudControlsAvailable {
                        ModeCloudBlock(store: store, mode: mode)
                    }
                    ModeVocabularyBlock(store: store, mode: mode, openVocabulary: openVocabulary)
                }
            }
        }
    }
}

struct ModeShortcutsBlock: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        Group {
            if let warning = store.bindingWarning {
                ModeInlineNotice("\(warning) Set their shortcuts here.")
            }
            if let saved = store.editingSaved {
                ModeShortcutRow(store: store, shortcut: saved.shortcuts.transcribe)
                ModeShortcutRow(store: store, shortcut: saved.shortcuts.switchTo)
            } else {
                ModeInlineNotice("Save this mode to give it shortcuts.")
            }
        }
    }
}

/// One chord: what it does, what it is now, and the two states of changing
/// it. A chord is only bound to a mode the core has stored, so this reads
/// the saved mode rather than the draft.
struct ModeShortcutRow: View {
    let store: ModesStore
    let shortcut: ModeShortcut

    private var isRecording: Bool { store.recording?.bindingId == shortcut.id }

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 4) {
                Text(shortcut.name).bodyText()
                Text(shortcut.description).metaText()
            }
        } trailing: {
            HStack(spacing: 10) {
                if isRecording {
                    let preview = store.recording?.preview ?? ""
                    Text(preview.isEmpty ? "Press keys…" : preview).metaText(Theme.accent)
                    Button("Cancel") { store.cancelRecording() }.buttonStyle(.quiet)
                } else {
                    ModeChord(shortcut.currentBinding)
                    Button("Change") { store.record(shortcut) }.buttonStyle(.secondary)
                    if shortcut.changed {
                        Button("Reset") { store.resetShortcut(shortcut) }.buttonStyle(.quiet)
                    }
                }
            }
        }
    }
}

/// A chord, or the fact that there is not one. An unbound mode is ordinary
/// — past nine chords the platform stops registering them — so the gap has
/// to read as a state rather than as nothing.
struct ModeChord: View {
    let binding: String

    init(_ binding: String) {
        self.binding = binding
    }

    var body: some View {
        if binding.isEmpty {
            Text("Not set").metaText()
        } else {
            Shortcut(binding)
        }
    }
}

struct ModeRewriteBlock: View {
    let store: ModesStore
    let mode: ModeDefinition
    var openProviders: () -> Void = {}

    private var providers: [ModePostProcessProvider] { store.settings?.postProcessProviders ?? [] }

    var body: some View {
        Group {
            ModeFieldRow(label: "Tone", disabled: !mode.llm.enabled) {
                Picker("", selection: store.field(\.tone, .balanced)) {
                    ForEach(ModeTone.allCases) { tone in
                        Text(tone.label).tag(tone)
                    }
                }
                .labelsHidden()
                .pickerStyle(.segmented)
                .disabled(!mode.llm.enabled)
            }
            ChoiceRow(
                title: "AI provider",
                choices: [ModeModelOption.inherit] + providers.map(\.id),
                label: { id in
                    id == ModeModelOption.inherit
                        ? store.inheritedProviderLabel
                        : providers.first { $0.id == id }?.label ?? id
                },
                selection: Binding(
                    get: { mode.llm.providerId ?? ModeModelOption.inherit },
                    set: { value in
                        store.edit { $0.llm.providerId = value == ModeModelOption.inherit ? nil : value }
                        store.loadCatalogIfNeeded()
                    }))
            .disabled(!mode.llm.enabled)
            if providers.isEmpty {
                ModeLinkNotice(
                    text: "No AI provider is configured yet. Add one in Settings before this mode can rewrite.",
                    action: "Open Providers",
                    perform: openProviders,
                    tone: .warning)
            } else if mode.llm.providerId != nil, store.explicitProvider == nil {
                ModeInlineNotice(
                    "This mode names a provider that is not configured on this install.", tone: .warning)
            }
            ModeLlmModelRow(store: store, mode: mode)
            ToggleRow(
                title: "Spoken instructions",
                detail: "End a dictation with \"Sona,\" and the rest of that sentence becomes an edit for the AI to apply, instead of words to type.",
                isOn: store.field(\.llm.spokenInstructions, false))
            .disabled(!mode.llm.enabled)
        }
    }
}

/// The model the rewrite runs on. Inherited it is read-only, because it is
/// the app's choice; named it is this provider's list, or free text when the
/// provider publishes none.
struct ModeLlmModelRow: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        ModeFieldRow(label: "AI model", disabled: !mode.llm.enabled) {
            if let destination = store.llmDestination, destination.inherited {
                Text(destination.modelId.isEmpty ? "No model selected" : destination.modelId)
                    .bodyText(14, Theme.inkSecondary)
            } else if store.explicitProviderIsFixed {
                Text(store.explicitProvider?.label ?? "Apple Intelligence").bodyText(14)
            } else {
                HStack(spacing: 8) {
                    InputField(prompt: "Model ID", text: store.field(\.llm.modelId, ""))
                        .disabled(!mode.llm.enabled)
                    if !catalogIds.isEmpty {
                        Menu {
                            ForEach(catalogIds, id: \.self) { id in
                                Button(id) { store.edit { $0.llm.modelId = id } }
                            }
                        } label: {
                            Text("Choose")
                        }
                        .menuStyle(.borderlessButton)
                        .fixedSize()
                    }
                    Button(store.catalogLoading ? "Loading…" : "Refresh") { store.discoverCatalog() }
                        .buttonStyle(.quiet)
                        .disabled(store.catalogLoading || mode.llm.providerId == nil)
                }
                if let status = store.catalog?.status {
                    Text(status).metaText()
                }
            }
        }
        // A named provider's list is worth having before the menu is
        // opened: an empty menu would read as "this provider has no
        // models" when nothing has asked yet.
        .task(id: mode.llm.providerId) { store.loadCatalogIfNeeded() }
    }

    private var catalogIds: [String] { store.catalog?.models.map(\.id) ?? [] }
}

struct ModeContextBlock: View {
    let store: ModesStore
    let mode: ModeDefinition
    var openPrivacy: () -> Void = {}

    var body: some View {
        ModeFieldRow(label: "Context level", hint: "This mode cannot exceed the privacy ceiling.") {
            Picker("", selection: store.field(\.contextPolicy, ModeContextPolicy.none)) {
                ForEach(ModeContextPolicy.allCases) { policy in
                    Text(policy.label).tag(policy)
                }
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            if store.contextClamped {
                ModeLinkNotice(
                    text: "Privacy limits this mode to \(store.contextCeiling.label).",
                    action: "Raise the ceiling in Privacy",
                    perform: openPrivacy,
                    tone: .warning)
            }
        }
    }
}

struct ModeRecognitionBlock: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        Group {
            ToggleRow(title: "Translate to English", isOn: store.field(\.asr.translateToEnglish, false))
            ToggleRow(
                title: "Literal punctuation",
                detail: "Convert spoken punctuation such as comma and question mark before vocabulary corrections.",
                isOn: store.field(\.asr.literalPunctuation, false))
            ToggleRow(title: "Remove filler words", isOn: store.field(\.asr.fillerWordRemovalEnabled, false))
            ToggleRow(title: "Voice activity detection", isOn: store.field(\.asr.vadEnabled, false))
        }
    }
}

struct ModeDeliveryBlock: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        Group {
            ChoiceRow(
                title: "Clipboard",
                choices: ModeClipboardHandling.allCases,
                label: \.label,
                selection: store.field(\.delivery.clipboardHandling, .dontModify))
            ToggleRow(title: "Auto-submit", isOn: store.field(\.delivery.autoSubmit, false))
            ChoiceRow(
                title: "Submit key",
                detail: mode.delivery.autoSubmit ? nil : "Turn on auto-submit to choose a key.",
                choices: ModeAutoSubmitKey.allCases,
                label: \.label,
                selection: store.field(\.delivery.autoSubmitKey, .enter))
            .disabled(!mode.delivery.autoSubmit)
            ToggleRow(title: "Append a trailing space", isOn: store.field(\.delivery.appendTrailingSpace, false))
            ModeFieldRow(label: "Paste delays", hint: "Milliseconds before and after delivery.") {
                HStack(spacing: 12) {
                    ModeNumberField(label: "Before", value: store.field(\.delivery.pasteDelayMs, 0))
                    ModeNumberField(label: "After", value: store.field(\.delivery.pasteDelayAfterMs, 0))
                }
            }
            ToggleRow(
                title: "Reliable paste",
                detail: "Restore the clipboard only after the app reads the transcript, where the system allows it.",
                isOn: store.field(\.delivery.reliablePaste, false))
        }
    }
}

/// Only reachable once the engine can actually run: a key and a current
/// acknowledgement.
struct ModeCloudBlock: View {
    let store: ModesStore
    let mode: ModeDefinition

    var body: some View {
        Group {
            ToggleRow(
                title: "Use local fallback",
                detail: "If the cloud provider cannot return a trustworthy result, transcribe the recording with a local model.",
                isOn: store.field(\.asr.localFallbackEnabled, true))
            if mode.asr.localFallbackEnabled {
                let named = store.modelOptions(for: mode.asr.localFallbackModelId ?? "")
                ChoiceRow(
                    title: "Fallback model",
                    choices: [ModeModelOption.ownLocalModel] + named.map(\.id),
                    label: { id in
                        id == ModeModelOption.ownLocalModel
                            ? "Use this mode's local model"
                            : named.first { $0.id == id }?.label ?? id
                    },
                    selection: Binding(
                        get: { mode.asr.localFallbackModelId ?? ModeModelOption.ownLocalModel },
                        set: { value in
                            store.edit {
                                $0.asr.localFallbackModelId = value == ModeModelOption.ownLocalModel ? nil : value
                            }
                        }))
            }
            ModeFieldRow(
                label: "Words to listen for",
                hint: "One word or name per line. Sona sends them to the provider with the audio."
            ) {
                ModeTextBox(
                    text: Binding(
                        get: { store.cloudKeytermsText },
                        set: { store.setCloudKeyterms($0) }),
                    prompt: "Product name\nPerson name",
                    lines: 3)
            }
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Word timestamps").bodyText()
                    Text("Required for cloud transcription and cannot be disabled.").metaText()
                }
            } trailing: {
                Text("On").metaText()
            }
        }
    }
}

/// The mode's own spelling pairs. The list everything shares is a tab of its
/// own; this one applies to this mode only, which is why the empty row says
/// so rather than sending the reader away.
struct ModeVocabularyBlock: View {
    let store: ModesStore
    let mode: ModeDefinition
    var openVocabulary: () -> Void = {}

    var body: some View {
        Group {
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Mode vocabulary").bodyText()
                    if mode.asr.customWords.isEmpty {
                        Text("This mode has no vocabulary of its own. Global vocabulary still applies.")
                            .metaText()
                    }
                }
            } trailing: {
                HStack(spacing: 8) {
                    Button("Global vocabulary", action: openVocabulary).buttonStyle(.quiet)
                    Button("Add pair") { store.addVocabularyRow() }.buttonStyle(.secondary)
                }
            }
            ForEach(store.vocabularyRows) { row in
                CardRow {
                    HStack(spacing: 8) {
                        InputField(
                            prompt: "What was said",
                            text: Binding(
                                get: { row.entry.spoken },
                                set: { store.setVocabulary(row.index, spoken: $0) }))
                        Image(systemName: "arrow.right")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.inkTertiary)
                        InputField(
                            prompt: "What to write",
                            text: Binding(
                                get: { row.entry.written },
                                set: { store.setVocabulary(row.index, written: $0) }))
                    }
                } trailing: {
                    Button("Remove") { store.removeVocabularyRow(row.index) }
                        .buttonStyle(.quiet)
                        .accessibilityLabel("Remove \(row.entry.spoken)")
                }
            }
            if store.vocabularyIncomplete {
                ModeInlineNotice("Complete or remove each vocabulary pair before saving this mode.", tone: .danger)
            }
        }
    }
}

// MARK: - Sheets

struct ModeDeleteSheet: View {
    let store: ModesStore
    let mode: Mode

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Delete mode").headlineText()
            Text("Delete \(mode.name)?").bodyText()
            Text("Its mode-only shortcuts will also be removed. This cannot be undone.").metaText()
            HStack {
                Spacer()
                Button("Cancel") { store.pendingDelete = nil }.buttonStyle(.secondary)
                Button("Delete") { store.confirmDelete() }.buttonStyle(.primary)
            }
            .padding(.top, 8)
        }
        .padding(24)
        .frame(width: 420)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
    }
}

/// The three acknowledgements are fixed for a provider, so this is a reading
/// task and one decision, not a form.
struct ModeConsentSheet: View {
    let store: ModesStore
    let provider: ModeCloudProvider

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Use \(provider.label) for cloud transcription").headlineText()
            Text("Review how this mode sends audio before enabling \(provider.label).").bodyText()
            Text("These acknowledgements are fixed for this provider. Declining keeps this mode on its current local engine.")
                .metaText()
            ModeConsentTerm(
                title: "Audio transfer",
                detail: "Recorded audio from this mode is sent directly to \(provider.label) for transcription.")
            ModeConsentTerm(
                title: "What the provider sees",
                detail: "The provider receives this mode's audio, its language, and the words you listed. What Sona reads from your other apps, and your keys, stay on this Mac.")
            ModeConsentTerm(
                title: "Local fallback",
                detail: "If the cloud provider cannot return a trustworthy result, Sona can use your selected local model.")
            if let failure = store.consentError {
                Text(failure).font(TypeScale.body(14)).foregroundStyle(Theme.live)
            }
            HStack {
                Spacer()
                Button("Keep local") { store.cancelConsent() }.buttonStyle(.secondary)
                Button(store.consentBusy ? "Accepting…" : "Accept and use cloud") { store.acceptConsent() }
                    .buttonStyle(.primary)
                    .disabled(store.consentBusy)
            }
            .padding(.top, 8)
        }
        .padding(24)
        .frame(width: 480)
        .background(Theme.page)
        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusDialog))
    }
}

struct ModeConsentTerm: View {
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(TypeScale.label(14)).foregroundStyle(Theme.ink)
            Text(detail).metaText()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(12)
        .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
    }
}

// MARK: - Small pieces

enum ModeNoticeTone {
    case plain, warning, danger

    var color: Color {
        switch self {
        case .plain: Theme.inkSecondary
        case .warning: Theme.accent
        case .danger: Theme.live
        }
    }
}

struct ModeNotice: View {
    let text: String
    var tone: ModeNoticeTone = .plain

    init(_ text: String, tone: ModeNoticeTone = .plain) {
        self.text = text
        self.tone = tone
    }

    var body: some View {
        Text(text)
            .font(TypeScale.body(14))
            .foregroundStyle(tone.color)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
    }
}

/// A notice that sits inside a card, between rows.
struct ModeInlineNotice: View {
    let text: String
    var tone: ModeNoticeTone = .plain

    init(_ text: String, tone: ModeNoticeTone = .plain) {
        self.text = text
        self.tone = tone
    }

    var body: some View {
        Text(text)
            .font(TypeScale.body(14))
            .foregroundStyle(tone.color)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
            .overlay(alignment: .bottom) { Hairline() }
    }
}

/// A notice whose fix is somewhere else in settings.
struct ModeLinkNotice: View {
    let text: String
    let action: String
    let perform: () -> Void
    var tone: ModeNoticeTone = .plain

    var body: some View {
        HStack(spacing: 8) {
            Text(text).font(TypeScale.body(14)).foregroundStyle(tone.color)
            Button(action, action: perform).buttonStyle(.quiet)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// A card row whose control needs the full width under its label.
struct ModeFieldRow<Content: View>: View {
    let label: String
    var hint: String?
    var disabled = false
    let content: Content

    init(
        label: String,
        hint: String? = nil,
        disabled: Bool = false,
        @ViewBuilder content: () -> Content
    ) {
        self.label = label
        self.hint = hint
        self.disabled = disabled
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(label).bodyText(15, disabled ? Theme.inkDisabled : Theme.ink)
            if let hint {
                Text(hint).metaText()
            }
            content
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { Hairline() }
    }
}

/// A multi-line text box with the same outline as the single-line field.
struct ModeTextBox: View {
    @Binding var text: String
    let prompt: String
    var lines: Int = 3

    var body: some View {
        ZStack(alignment: .topLeading) {
            if text.isEmpty {
                Text(prompt)
                    .font(TypeScale.body())
                    .foregroundStyle(Theme.inkTertiary)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .allowsHitTesting(false)
            }
            TextEditor(text: $text)
                .font(TypeScale.body())
                .foregroundStyle(Theme.ink)
                .scrollContentBackground(.hidden)
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
        }
        .frame(height: CGFloat(lines) * 22 + 20)
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
    }
}

/// A labelled millisecond field.
struct ModeNumberField: View {
    let label: String
    @Binding var value: Int

    var body: some View {
        HStack(spacing: 8) {
            Text(label).metaText()
            TextField("", value: $value, format: .number)
                .textFieldStyle(.plain)
                .font(TypeScale.body())
                .foregroundStyle(Theme.ink)
                .frame(width: 72)
                .padding(.horizontal, 10)
                .frame(height: 32)
                .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl).strokeBorder(Theme.border, lineWidth: 1))
        }
    }
}

// MARK: - Draft bindings

extension ModesStore {
    /// A binding straight into the draft. Reading falls back when there is
    /// no mode open, which only happens for a frame while the list loads.
    func field<Value>(_ path: WritableKeyPath<ModeDefinition, Value>, _ fallback: Value) -> Binding<Value> {
        Binding(
            get: { self.editing?[keyPath: path] ?? fallback },
            set: { next in self.edit { $0[keyPath: path] = next } })
    }
}

// MARK: - Languages

/// The languages a mode can be set to, as the web app lists them: the same
/// order, and without the bare Chinese code the picker drops in favour of
/// the two scripts.
struct ModeLanguage: Identifiable, Hashable {
    let code: String
    let label: String

    var id: String { code }

    init(_ code: String, _ label: String) {
        self.code = code
        self.label = label
    }

    static func name(_ code: String) -> String {
        all.first { $0.code == code }?.label ?? code
    }

    static let all: [ModeLanguage] = [
        ModeLanguage("auto", "Auto detect"),
        ModeLanguage("en", "English"),
        ModeLanguage("zh-Hans", "Chinese (Simplified)"),
        ModeLanguage("zh-Hant", "Chinese (Traditional)"),
        ModeLanguage("yue", "Cantonese"),
        ModeLanguage("de", "German"),
        ModeLanguage("es", "Spanish"),
        ModeLanguage("ru", "Russian"),
        ModeLanguage("ko", "Korean"),
        ModeLanguage("fr", "French"),
        ModeLanguage("ja", "Japanese"),
        ModeLanguage("pt", "Portuguese"),
        ModeLanguage("tr", "Turkish"),
        ModeLanguage("pl", "Polish"),
        ModeLanguage("ca", "Catalan"),
        ModeLanguage("nl", "Dutch"),
        ModeLanguage("ar", "Arabic"),
        ModeLanguage("sv", "Swedish"),
        ModeLanguage("it", "Italian"),
        ModeLanguage("id", "Indonesian"),
        ModeLanguage("hi", "Hindi"),
        ModeLanguage("fi", "Finnish"),
        ModeLanguage("vi", "Vietnamese"),
        ModeLanguage("he", "Hebrew"),
        ModeLanguage("uk", "Ukrainian"),
        ModeLanguage("el", "Greek"),
        ModeLanguage("ms", "Malay"),
        ModeLanguage("cs", "Czech"),
        ModeLanguage("ro", "Romanian"),
        ModeLanguage("da", "Danish"),
        ModeLanguage("hu", "Hungarian"),
        ModeLanguage("ta", "Tamil"),
        ModeLanguage("no", "Norwegian"),
        ModeLanguage("th", "Thai"),
        ModeLanguage("ur", "Urdu"),
        ModeLanguage("hr", "Croatian"),
        ModeLanguage("bg", "Bulgarian"),
        ModeLanguage("lt", "Lithuanian"),
        ModeLanguage("la", "Latin"),
        ModeLanguage("mi", "Maori"),
        ModeLanguage("ml", "Malayalam"),
        ModeLanguage("cy", "Welsh"),
        ModeLanguage("sk", "Slovak"),
        ModeLanguage("te", "Telugu"),
        ModeLanguage("fa", "Persian"),
        ModeLanguage("lv", "Latvian"),
        ModeLanguage("bn", "Bengali"),
        ModeLanguage("sr", "Serbian"),
        ModeLanguage("az", "Azerbaijani"),
        ModeLanguage("sl", "Slovenian"),
        ModeLanguage("kn", "Kannada"),
        ModeLanguage("et", "Estonian"),
        ModeLanguage("mk", "Macedonian"),
        ModeLanguage("br", "Breton"),
        ModeLanguage("eu", "Basque"),
        ModeLanguage("is", "Icelandic"),
        ModeLanguage("hy", "Armenian"),
        ModeLanguage("ne", "Nepali"),
        ModeLanguage("mn", "Mongolian"),
        ModeLanguage("bs", "Bosnian"),
        ModeLanguage("kk", "Kazakh"),
        ModeLanguage("sq", "Albanian"),
        ModeLanguage("sw", "Swahili"),
        ModeLanguage("gl", "Galician"),
        ModeLanguage("mr", "Marathi"),
        ModeLanguage("pa", "Punjabi"),
        ModeLanguage("si", "Sinhala"),
        ModeLanguage("km", "Khmer"),
        ModeLanguage("sn", "Shona"),
        ModeLanguage("yo", "Yoruba"),
        ModeLanguage("so", "Somali"),
        ModeLanguage("af", "Afrikaans"),
        ModeLanguage("oc", "Occitan"),
        ModeLanguage("ka", "Georgian"),
        ModeLanguage("be", "Belarusian"),
        ModeLanguage("tg", "Tajik"),
        ModeLanguage("sd", "Sindhi"),
        ModeLanguage("gu", "Gujarati"),
        ModeLanguage("am", "Amharic"),
        ModeLanguage("yi", "Yiddish"),
        ModeLanguage("lo", "Lao"),
        ModeLanguage("uz", "Uzbek"),
        ModeLanguage("fo", "Faroese"),
        ModeLanguage("ht", "Haitian Creole"),
        ModeLanguage("ps", "Pashto"),
        ModeLanguage("tk", "Turkmen"),
        ModeLanguage("nn", "Nynorsk"),
        ModeLanguage("mt", "Maltese"),
        ModeLanguage("sa", "Sanskrit"),
        ModeLanguage("lb", "Luxembourgish"),
        ModeLanguage("my", "Myanmar"),
        ModeLanguage("bo", "Tibetan"),
        ModeLanguage("tl", "Tagalog"),
        ModeLanguage("mg", "Malagasy"),
        ModeLanguage("as", "Assamese"),
        ModeLanguage("tt", "Tatar"),
        ModeLanguage("haw", "Hawaiian"),
        ModeLanguage("ln", "Lingala"),
        ModeLanguage("ha", "Hausa"),
        ModeLanguage("ba", "Bashkir"),
        ModeLanguage("jw", "Javanese"),
        ModeLanguage("su", "Sundanese"),
    ]
}

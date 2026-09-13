import Foundation
import Observation

extension CoreEvent {
    /// The whole snapshot, every time anything about a mode changes.
    static let modesChanged = "modes-changed-event"
    /// Carries nothing usable; the answer is to ask for the settings again.
    static let modesSettingsChanged = "settings-changed"
}

/// Every mode, the one in force, and the editor's unsaved draft.
///
/// The core owns the list and hands back a whole snapshot from every
/// mutation, so this store never patches its own copy: it sends the command
/// and adopts the answer. The revision travels with each mutation, which is
/// what turns a settings window open in two places from a silent overwrite
/// into a refusal this store can explain.
@MainActor @Observable final class ModesStore {
    private let core: Core

    // What the core says.
    private(set) var error: String?
    private(set) var loaded = false
    private(set) var modes: [Mode] = []
    private(set) var activeModeId = Mode.defaultId
    private(set) var revision: UInt64 = 0
    private(set) var appRules: [ModeActivationRule] = []
    private(set) var websiteRules: [ModeWebsiteRule] = []
    private(set) var settings: ModeAppSettings?
    private(set) var models: [ModelInfo] = []
    /// Cloud engine value to "a key is saved for it".
    private(set) var configuredKeys: [String: Bool] = [:]

    // What the editor is doing.
    private(set) var draft: ModeDefinition?
    /// Stable identities for the vocabulary rows, so deleting the second row
    /// does not make the third row's text field lose focus and inherit the
    /// second one's contents.
    private(set) var vocabularyRowIds: [UUID] = []
    private(set) var busy = false
    /// Set when the core refused on revision: the list below has been
    /// reloaded, and the draft is the user's, unsaved.
    private(set) var conflict = false

    // Rewrite model discovery, for a provider the mode names itself.
    private(set) var catalog: ModeLlmCatalog?
    private(set) var catalogLoading = false
    private var catalogProviderId: String?

    // Chord recording.
    private(set) var recording: ModeRecording?

    // Sheets.
    var pendingDelete: Mode?
    var pendingConsent: ModeCloudProvider?
    private(set) var consentError: String?
    private(set) var consentBusy = false

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.modesChanged) { [weak self] line in
            guard let snapshot = try? Core.decoder.decode(ModesSnapshot.self, from: line) else { return }
            self?.adopt(snapshot)
        }
        core.observe(CoreEvent.modesSettingsChanged) { [weak self] _ in
            self?.refreshSettings()
        }
        core.observe(CoreEvent.modelsUpdated) { [weak self] _ in
            self?.refreshModels()
        }
        core.observe(CoreEvent.handyKeys) { [weak self] line in
            guard let event = try? Core.decoder.decode(HandyKeysEvent.self, from: line) else { return }
            self?.handle(event)
        }
    }

    // MARK: - Loading

    func start() async {
        await load()
    }

    /// Re-read everything the editor renders against. Also the way out of a
    /// revision conflict: the draft survives, the list under it is current.
    func reload() {
        Task { await load() }
    }

    private func load() async {
        do {
            async let snapshot: ModesSnapshot = core.request("get_modes")
            async let settings: ModeAppSettings = core.request("get_app_settings")
            async let models: [ModelInfo] = core.request("get_available_models")
            adopt(try await snapshot)
            self.settings = try await settings
            self.models = try await models
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loaded = true
        await loadSecrets()
    }

    /// Whether each cloud provider has a key, which decides between "Add a
    /// key" and simply switching the engine on.
    private func loadSecrets() async {
        for provider in ModeCloudProvider.all {
            let state: ModeSecretState? = try? await core.request(
                "get_provider_secret_state",
                ModeSecretQuery(kind: "stt", providerId: provider.secretAccount))
            configuredKeys[provider.engine.rawValue] = state?.configured ?? false
        }
    }

    private func refreshSettings() {
        Task {
            if let next: ModeAppSettings = try? await core.request("get_app_settings") {
                settings = next
            }
        }
    }

    private func refreshModels() {
        Task {
            if let next: [ModelInfo] = try? await core.request("get_available_models") {
                models = next
            }
        }
    }

    private func adopt(_ snapshot: ModesSnapshot) {
        modes = snapshot.modes
        activeModeId = snapshot.activeModeId
        revision = snapshot.revision
        appRules = snapshot.modeActivationRules
        websiteRules = snapshot.modeWebsiteActivationRules
        if draft == nil {
            resetRowIds()
        }
    }

    // MARK: - Reading the list

    var activeMode: Mode? { modes.first { $0.id == activeModeId } }

    /// What the editor shows: the mode being edited, or the active one when
    /// nothing has been picked. Opening settings lands on the mode in force.
    var editing: ModeDefinition? {
        if let draft { return draft }
        return activeMode.map(ModeDefinition.init)
    }

    /// The saved mode behind the draft, or nil while the draft is a mode the
    /// core has not accepted yet.
    var editingSaved: Mode? {
        guard let id = editing?.id else { return nil }
        return modes.first { $0.id == id }
    }

    var dirty: Bool {
        guard let editing, let saved = editingSaved else { return draft != nil }
        return ModeDefinition(saved) != editing
    }

    /// A pair with one side blank corrects nothing and cannot be saved; a
    /// brand-new empty row counts, which is what stops a stray Add.
    var vocabularyIncomplete: Bool {
        (editing?.asr.customWords ?? []).contains {
            $0.spoken.trimmingCharacters(in: .whitespaces).isEmpty
                || $0.written.trimmingCharacters(in: .whitespaces).isEmpty
        }
    }

    /// The reason Save is refused, or nil when it is allowed. Name first,
    /// because it is the one a person can be halfway through typing.
    var blockingReason: String? {
        guard let editing else { return nil }
        if editing.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return ModeMutationError.emptyName.message
        }
        if vocabularyIncomplete {
            return "Complete or remove each vocabulary pair before saving this mode."
        }
        // An empty per-mode model means "use the app's", so it is only a
        // missing fallback when the app has nothing selected either.
        if cloudControlsAvailable,
           editing.asr.localFallbackEnabled,
           editing.asr.modelId.trimmingCharacters(in: .whitespaces).isEmpty,
           (editing.asr.localFallbackModelId ?? "").trimmingCharacters(in: .whitespaces).isEmpty,
           (settings?.selectedModel ?? "").trimmingCharacters(in: .whitespaces).isEmpty {
            return "Choose a fallback model or set this mode's local model before saving."
        }
        return nil
    }

    var canSave: Bool { dirty && blockingReason == nil && !busy }

    /// The models the picker can offer: what is on disk, plus the mode's own
    /// choice when this install no longer has it, so opening the editor
    /// cannot silently repoint a mode at something else.
    func modelOptions(for modelId: String) -> [ModeModelOption] {
        var options = models.filter(\.isDownloaded).map { ModeModelOption(id: $0.id, label: $0.name) }
        if !modelId.isEmpty, !options.contains(where: { $0.id == modelId }) {
            options.insert(ModeModelOption(id: modelId, label: modelId), at: 0)
        }
        return options
    }

    /// "Default: the global model (Parakeet v3)" — the model an empty choice
    /// actually runs, not the word "default".
    var inheritedModelLabel: String {
        let globalId = settings?.selectedModel ?? ""
        if let named = models.first(where: { $0.id == globalId }) {
            return "Default: the global model (\(named.name))"
        }
        return "Default: the global model"
    }

    var inheritedProviderLabel: String {
        let globalId = settings?.postProcessProviderId ?? ""
        if let named = settings?.postProcessProviders.first(where: { $0.id == globalId }) {
            return "Default: the global provider (\(named.label))"
        }
        return "Default: the global provider"
    }

    /// Where this draft's rewrite will actually go.
    var llmDestination: ModeLlmDestination? {
        editing.map { ModeLlmDestination($0.llm, settings) }
    }

    /// The provider the mode names for itself, when it names one this install
    /// still has.
    var explicitProvider: ModePostProcessProvider? {
        guard let id = editing?.llm.providerId else { return nil }
        return settings?.postProcessProviders.first { $0.id == id }
    }

    /// Apple Intelligence publishes no model list and takes no model ID.
    var explicitProviderIsFixed: Bool { editing?.llm.providerId == "apple_intelligence" }

    /// The ceiling clamps what a mode may ask for; the mode keeps its own
    /// setting, but only this much of it happens.
    var contextCeiling: ModeContextPolicy { settings?.contextPolicyCeiling ?? .none }

    var contextClamped: Bool {
        guard let editing else { return false }
        return editing.contextPolicy.isAbove(contextCeiling)
    }

    /// Website rules read the address bar, which only happens when URL
    /// capture is on.
    var websiteCaptureAllowed: Bool { settings?.contextUrlCaptureEnabled ?? false }

    func hasKey(_ provider: ModeCloudProvider) -> Bool {
        configuredKeys[provider.engine.rawValue] ?? false
    }

    func hasConsent(_ provider: ModeCloudProvider) -> Bool {
        settings?.hasCurrentConsent(provider) ?? false
    }

    /// The cloud panel — fallback, keyterms, timestamps — opens only for an
    /// engine that can actually run: a key in the keyring and a current
    /// acknowledgement. Neither is a mode setting, so neither is in the
    /// draft, and a mode can sit pointed at a cloud engine it cannot use.
    var cloudControlsAvailable: Bool {
        guard let provider = editing?.asr.requestedEngine.cloud else { return false }
        return hasKey(provider) && hasConsent(provider)
    }

    /// The provider the draft's engine names, when it names one.
    var selectedCloudProvider: ModeCloudProvider? { editing?.asr.requestedEngine.cloud }

    /// The sentence under an engine that is present but not usable.
    func cloudSetupNote(_ provider: ModeCloudProvider) -> String {
        "\(provider.label) needs an API key in the system credential store and a current transfer acknowledgement before its cloud controls open up."
    }

    /// The app rules and website rules for one mode, as one list.
    func activationItems(_ modeId: String) -> [ModeActivationItem] {
        var items = appRules.filter { $0.modeId == modeId }.map {
            ModeActivationItem(id: "app:\($0.appId)", target: $0.appId, detail: nil)
        }
        items += websiteRules.filter { $0.modeId == modeId }.map {
            ModeActivationItem(
                id: "site:\($0.matchKind.rawValue):\($0.host)",
                target: $0.host,
                detail: $0.matchKind.label)
        }
        return items
    }

    func actions(for mode: Mode) -> [ModeRowAction] {
        let index = modes.firstIndex(where: { $0.id == mode.id }) ?? 0
        return ModeRowAction.all(
            for: mode, index: index, count: modes.count,
            isActive: mode.id == activeModeId, busy: busy)
    }

    /// After nine chords the platform stops handing out registrations, and a
    /// new mode's shortcuts silently do nothing. Saying so beats a dead key.
    var bindingWarning: String? {
        modes.count > 9 ? "New modes may start unbound after the first nine." : nil
    }

    // MARK: - Editing the draft

    func select(_ mode: Mode) {
        draft = ModeDefinition(mode)
        conflict = false
        resetRowIds()
        loadCatalogIfNeeded()
    }

    func discard() {
        draft = nil
        conflict = false
        resetRowIds()
    }

    /// Change the draft. Starts from the active mode when the editor is
    /// showing it, so typing in an untouched editor begins an edit rather
    /// than dropping the keystroke.
    func edit(_ change: (inout ModeDefinition) -> Void) {
        guard var next = editing else { return }
        let before = next.llm.providerId
        change(&next)
        if next.llm.providerId != before {
            catalog = nil
            catalogProviderId = nil
        }
        draft = next
    }

    private func resetRowIds() {
        vocabularyRowIds = (editing?.asr.customWords ?? []).map { _ in UUID() }
    }

    /// The draft's vocabulary, paired with a stable identity per row.
    var vocabularyRows: [ModeVocabularyRow] {
        let entries = editing?.asr.customWords ?? []
        var rows: [ModeVocabularyRow] = []
        rows.reserveCapacity(entries.count)
        for (index, entry) in entries.enumerated() {
            let id = index < vocabularyRowIds.count ? vocabularyRowIds[index] : UUID()
            rows.append(ModeVocabularyRow(id: id, index: index, entry: entry))
        }
        return rows
    }

    func addVocabularyRow() {
        edit { $0.asr.customWords.append(ModeVocabularyEntry()) }
        vocabularyRowIds.append(UUID())
    }

    func setVocabulary(_ index: Int, spoken: String? = nil, written: String? = nil) {
        edit {
            guard index < $0.asr.customWords.count else { return }
            if let spoken { $0.asr.customWords[index].spoken = spoken }
            if let written { $0.asr.customWords[index].written = written }
        }
    }

    func removeVocabularyRow(_ index: Int) {
        edit {
            guard index < $0.asr.customWords.count else { return }
            $0.asr.customWords.remove(at: index)
        }
        if index < vocabularyRowIds.count {
            vocabularyRowIds.remove(at: index)
        }
    }

    /// The words a cloud provider is told to listen for, one per line.
    var cloudKeytermsText: String {
        (editing?.asr.cloudKeyterms ?? []).joined(separator: "\n")
    }

    func setCloudKeyterms(_ text: String) {
        let terms = text.split(whereSeparator: \.isNewline)
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        edit { $0.asr.cloudKeyterms = terms }
    }

    // MARK: - Mutations

    /// Every revisioned command goes through here: one in flight at a time,
    /// the answer replaces the list, and a stale revision reloads instead of
    /// leaving the editor arguing with a list that has already moved.
    private func mutate(
        _ work: @escaping () async throws -> ModesSnapshot,
        then follow: ((ModesSnapshot) -> Void)? = nil
    ) {
        guard !busy else { return }
        busy = true
        Task {
            do {
                let snapshot = try await work()
                adopt(snapshot)
                follow?(snapshot)
                error = nil
                conflict = false
                // A mode mutation can move app-wide settings with it, and
                // the editor reads several of them.
                refreshSettings()
            } catch let failure as CoreError {
                if let refusal = failure.remote(as: ModeMutationError.self) {
                    handle(refusal)
                } else {
                    error = failure.localizedDescription
                }
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }

    /// A stale revision is not an error the user caused: the list underneath
    /// moved. Reload it, keep the draft, and let the banner say so.
    private func handle(_ refusal: ModeMutationError) {
        if case .staleRevision = refusal {
            conflict = true
            reload()
            return
        }
        error = refusal.message
    }

    func activate(_ mode: Mode) {
        guard mode.id != activeModeId else { return }
        mutate { [core] in try await core.request("set_active_mode", ModeIdRequest(modeId: mode.id)) }
    }

    func save() {
        guard let editing, canSave else { return }
        var mode = editing
        // A cloud transcript arrives without timing unless it is asked for,
        // and the rest of Sona expects word times, so the cloud engines
        // cannot be saved without them.
        if mode.asr.requestedEngine != .local {
            mode.asr.cloudTimestamps = true
            draft = mode
        }
        conflict = false
        let expected = revision
        mutate {
            [core] in try await core.request(
                "upsert_mode", ModeUpsertRequest(mode: try mode.params(), expectedRevision: expected))
        } then: { [weak self] snapshot in
            // Adopt the stored mode, so a name the core normalised does not
            // read as an unsaved change the moment it is saved.
            guard let self, let saved = snapshot.modes.first(where: { $0.id == mode.id }) else { return }
            self.draft = ModeDefinition(saved)
        }
    }

    /// Duplicate a mode and open the copy. The "New mode" button duplicates
    /// the default one, so there is one path and it always starts from
    /// something that works.
    func duplicate(_ source: Mode) {
        var copy = ModeDefinition(source)
        copy.id = "mode-\(UUID().uuidString.lowercased())"
        copy.name = "\(source.name) copy"
        let expected = revision
        draft = copy
        conflict = false
        resetRowIds()
        mutate {
            [core] in try await core.request(
                "upsert_mode", ModeUpsertRequest(mode: try copy.params(), expectedRevision: expected))
        } then: { [weak self] snapshot in
            guard let self, let created = snapshot.modes.first(where: { $0.id == copy.id }) else { return }
            self.draft = ModeDefinition(created)
            self.resetRowIds()
        }
    }

    func createMode() {
        guard let source = modes.first(where: { $0.id == Mode.defaultId }) ?? modes.first else { return }
        duplicate(source)
    }

    func confirmDelete() {
        guard let mode = pendingDelete else { return }
        pendingDelete = nil
        let expected = revision
        if draft?.id == mode.id {
            draft = nil
        }
        mutate { [core] in
            try await core.request("delete_mode", ModeRevisionRequest(modeId: mode.id, expectedRevision: expected))
        }
    }

    func move(_ mode: Mode, by direction: Int) {
        let ids = ModesOrder.withMove(modes.map(\.id), mode.id, by: direction)
        reorder(ids)
    }

    func move(_ mode: Mode, to index: Int) {
        let ids = ModesOrder.withMove(modes.map(\.id), mode.id, to: index)
        reorder(ids)
    }

    private func reorder(_ ids: [String]) {
        guard ids != modes.map(\.id) else { return }
        let expected = revision
        mutate { [core] in
            try await core.request("reorder_modes", ModeReorderRequest(orderedIds: ids, expectedRevision: expected))
        }
    }

    // MARK: - Activation rules

    /// Bind the app in front to this mode. The core reads the frontmost
    /// application itself, which is why this takes no app: whatever is in
    /// front when the button is pressed is the answer, and Sona's own window
    /// is not it.
    func captureApp(for modeId: String) {
        let expected = revision
        mutate { [core] in
            try await core.request(
                "capture_mode_activation_rule",
                ModeRevisionRequest(modeId: modeId, expectedRevision: expected))
        }
    }

    func removeAppRule(_ appId: String) {
        let expected = revision
        mutate { [core] in
            try await core.request(
                "remove_mode_activation_rule",
                ModeAppRuleRequest(appId: appId, expectedRevision: expected))
        }
    }

    func captureWebsite(for modeId: String, match: ModeWebsiteHostMatch) {
        let expected = revision
        mutate { [core] in
            try await core.request(
                "capture_mode_website_activation_rule",
                ModeWebsiteCaptureRequest(
                    modeId: modeId, matchKind: match.rawValue, expectedRevision: expected))
        }
    }

    func removeWebsiteRule(_ host: String, match: ModeWebsiteHostMatch) {
        let expected = revision
        mutate { [core] in
            try await core.request(
                "remove_mode_website_activation_rule",
                ModeWebsiteRuleRequest(
                    host: host, matchKind: match.rawValue, expectedRevision: expected))
        }
    }

    // MARK: - Cloud consent

    /// Turn the engine on, or stop at the sheet when audio may not leave
    /// yet. An engine with no key is not selectable at all: there is
    /// nothing to consent to until a key exists.
    func chooseEngine(_ engine: ModeRequestedEngine) {
        guard let provider = engine.cloud else {
            edit { $0.asr.requestedEngine = .local }
            return
        }
        guard hasKey(provider) else { return }
        if hasConsent(provider) {
            selectCloud(provider)
            return
        }
        consentError = nil
        pendingConsent = provider
    }

    /// Switching to a cloud engine forces one setting with it: a cloud
    /// transcript without word times is no use to the rest of Sona.
    private func selectCloud(_ provider: ModeCloudProvider) {
        edit {
            $0.asr.requestedEngine = provider.engine
            $0.asr.cloudTimestamps = true
        }
    }

    /// Record the acknowledgement, then switch the engine. The draft only
    /// moves after the core has the consent, so backing out of the sheet
    /// leaves the mode exactly as it was.
    func acceptConsent() {
        guard let provider = pendingConsent, !consentBusy else { return }
        consentBusy = true
        consentError = nil
        Task {
            do {
                let _: ModeConsentResult = try await core.request(
                    "accept_cloud_stt_provider_consent",
                    ModeConsentRequest(provider: provider.engine.rawValue))
                refreshSettings()
                selectCloud(provider)
                pendingConsent = nil
            } catch let failure as CoreError {
                consentError = failure.remote(as: String.self) == "unknown_provider"
                    ? "This cloud provider is not available in this build."
                    : "The acknowledgement could not be saved."
            } catch {
                consentError = "The acknowledgement could not be saved."
            }
            consentBusy = false
        }
    }

    func cancelConsent() {
        pendingConsent = nil
        consentError = nil
    }

    // MARK: - Rewrite model discovery

    func loadCatalogIfNeeded() {
        guard let providerId = editing?.llm.providerId,
              providerId != "apple_intelligence",
              catalogProviderId != providerId,
              !catalogLoading
        else { return }
        discoverCatalog()
    }

    func discoverCatalog() {
        guard let providerId = editing?.llm.providerId, !catalogLoading else { return }
        catalogLoading = true
        catalogProviderId = providerId
        Task {
            do {
                catalog = try await core.request(
                    "discover_post_process_model_catalog",
                    ModeProviderRequest(providerId: providerId))
            } catch {
                catalog = nil
                self.error = error.localizedDescription
            }
            catalogLoading = false
        }
    }

    // MARK: - Chords

    /// Start listening for a chord. The core streams raw key events while a
    /// recording is open, and refuses outright when macOS Secure Input has
    /// the keyboard.
    func record(_ shortcut: ModeShortcut) {
        guard recording == nil else { return }
        recording = ModeRecording(bindingId: shortcut.id, original: shortcut.currentBinding)
        Task {
            do {
                try await core.request("start_handy_keys_recording", ModeBindingRequest(bindingId: shortcut.id))
            } catch {
                recording = nil
                self.error = error.localizedDescription.contains("secure-input-active")
                    ? "macOS Secure Input is blocking key events, so shortcuts cannot be recorded. Resolve the Secure Input warning first."
                    : error.localizedDescription
            }
        }
    }

    func cancelRecording() {
        guard recording != nil else { return }
        recording = nil
        Task { try? await core.request("stop_handy_keys_recording") }
    }

    /// A chord is whatever was held when the first key came back up. Holding
    /// modifiers alone is a chord too, which is why the last modifier-only
    /// string is kept: it is the answer when nothing else was pressed.
    private func handle(_ event: HandyKeysEvent) {
        guard var live = recording else { return }
        if event.isKeyDown {
            if !event.hotkeyString.isEmpty {
                if event.key != nil {
                    live.keyed = event.hotkeyString
                } else {
                    live.modifierOnly = event.hotkeyString
                }
                live.preview = event.hotkeyString
                recording = live
            }
            return
        }
        if event.key != nil {
            commit(live.keyed.isEmpty ? event.hotkeyString : live.keyed)
        } else if event.modifiers.isEmpty, live.keyed.isEmpty, !live.modifierOnly.isEmpty {
            commit(live.modifierOnly)
        }
    }

    private func commit(_ binding: String) {
        guard let live = recording, !binding.isEmpty else { return }
        recording = nil
        Task {
            try? await core.request("stop_handy_keys_recording")
            await write(binding, to: live.bindingId)
        }
    }

    func resetShortcut(_ shortcut: ModeShortcut) {
        Task {
            do {
                let response: BindingChange = try await core.request(
                    "reset_binding", ModeBindingIdRequest(id: shortcut.id))
                if response.success {
                    await refreshModes()
                    error = nil
                } else {
                    error = response.error ?? "That shortcut could not be reset."
                }
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    private func write(_ binding: String, to id: String) async {
        do {
            let response: BindingChange = try await core.request(
                "change_binding", ModeChangeBindingRequest(id: id, binding: binding))
            if response.success {
                await refreshModes()
                error = nil
            } else {
                error = response.error ?? "That shortcut is already taken."
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// The chords live in the app settings, so the list has to be asked
    /// again before a new chord shows on its row.
    private func refreshModes() async {
        if let snapshot: ModesSnapshot = try? await core.request("get_modes") {
            adopt(snapshot)
        }
    }
}

// MARK: - Recording state

/// One chord being recorded: what to write it to, what was there before, and
/// the best answer so far.
struct ModeRecording: Equatable {
    let bindingId: String
    let original: String
    /// The last chord that included a real key.
    var keyed = ""
    /// The last chord of modifiers alone.
    var modifierOnly = ""
    /// What to show while the keys are still down.
    var preview = ""
}

/// A vocabulary row with an identity that survives its neighbours moving.
struct ModeVocabularyRow: Identifiable {
    let id: UUID
    let index: Int
    let entry: ModeVocabularyEntry
}

/// One entry in the speech model picker.
struct ModeModelOption: Identifiable, Hashable {
    let id: String
    let label: String

    /// The value that means "whatever the app is set to". Matches the web
    /// app's sentinel so both ends agree on what an empty model means.
    static let inherit = "__mode_inherit_global__"
    /// The value that means "this mode's own local model", for the cloud
    /// fallback picker.
    static let ownLocalModel = "__mode_local_model__"
}

// MARK: - Request shapes
//
// The request encoder converts no keys, so these are camelCase exactly as
// the dispatcher spells them.

private struct ModeIdRequest: Encodable {
    let modeId: String
}

private struct ModeUpsertRequest: Encodable {
    let mode: JSONValue
    let expectedRevision: UInt64
}

private struct ModeRevisionRequest: Encodable {
    let modeId: String
    let expectedRevision: UInt64
}

private struct ModeReorderRequest: Encodable {
    let orderedIds: [String]
    let expectedRevision: UInt64
}

private struct ModeAppRuleRequest: Encodable {
    let appId: String
    let expectedRevision: UInt64
}

private struct ModeWebsiteCaptureRequest: Encodable {
    let modeId: String
    let matchKind: String
    let expectedRevision: UInt64
}

private struct ModeWebsiteRuleRequest: Encodable {
    let host: String
    let matchKind: String
    let expectedRevision: UInt64
}

private struct ModeSecretQuery: Encodable {
    let kind: String
    let providerId: String
}

private struct ModeProviderRequest: Encodable {
    let providerId: String
}

private struct ModeConsentRequest: Encodable {
    let provider: String
}

/// `accept_cloud_stt_provider_consent` answers with the whole cloud block;
/// the settings are re-read anyway, so only the acceptance matters here.
private struct ModeConsentResult: Decodable {}

private struct ModeBindingRequest: Encodable {
    let bindingId: String
}

private struct ModeBindingIdRequest: Encodable {
    let id: String
}

private struct ModeChangeBindingRequest: Encodable {
    let id: String
    let binding: String
}

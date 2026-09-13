import Foundation

extension CoreEvent {
    /// Any settings write, from this screen or another window. The payload
    /// names nothing useful, so the answer is always to read the settings back.
    static let providersSettingsChanged = "settings-changed"
}

/// The providers screen: which language model rewrites a transcript, where it
/// lives, what key reaches it, what the reader has allowed to leave this Mac,
/// and the keys for the cloud transcription routes.
///
/// The core owns all of it. Nothing here caches a decision: every write is a
/// command, and the settings are read back afterwards because these commands
/// do not announce themselves.
@MainActor
@Observable
final class ProvidersStore {
    /// The settings as the core last reported them.
    private(set) var settings: ProvidersSettings?
    /// The last thing this screen could not do.
    private(set) var error: String?

    /// The last model lookup per provider-and-address.
    private(set) var catalogs: [String: PostProcessCatalogEntry] = [:]

    /// Key state probed directly, which is fresher than the settings copy: the
    /// settings are written when a key changes, the store answers now.
    private(set) var llmSecrets: [String: SecretState] = [:]
    private(set) var cloudSecrets: [String: SecretState] = [:]
    /// What one cloud row could not do, shown in that row.
    private(set) var cloudErrors: [String: String] = [:]
    /// Cloud rows still waiting on their first answer from the store.
    private(set) var cloudChecking: Set<String> = []

    /// Apple Intelligence answered "not on this Mac" when it was selected.
    private(set) var appleIntelligenceUnavailable = false
    /// What the acknowledgement sheet could not do, shown inside the sheet.
    private(set) var consentError: String?
    /// What the prompt editor could not do, shown inside the sheet.
    private(set) var promptError: String?
    /// The address was rewritten and the new one is not acknowledged yet.
    private(set) var endpointChanged = false

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var working: Set<String> = []
    /// The signature of the last automatic lookup, so a settings read does not
    /// ask the provider the same question again.
    @ObservationIgnored private var lastAutoDiscovery: String?
    /// The provider whose key state was last probed.
    @ObservationIgnored private var lastProbedProvider: String?
    @ObservationIgnored private var refreshTask: Task<Void, Never>?

    private enum Busy {
        static let provider = "provider"
        static let enabled = "enabled"
        static let prompt = "prompt"
        static let consent = "consent"
        static func baseUrl(_ id: String) -> String { "base-url:" + id }
        static func model(_ id: String) -> String { "model:" + id }
        static func secret(_ id: String) -> String { "secret:" + id }
        static func catalog(_ scope: String) -> String { "catalog:" + scope }
        static func cloud(_ account: String) -> String { "cloud:" + account }
    }

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.providersSettingsChanged) { [weak self] _ in
            self?.scheduleRefresh()
        }
    }

    /// The first read: the settings, then the two cloud accounts, then whatever
    /// the selected provider needs.
    func start() async {
        await refresh()
        await refreshCloudSecretStates()
        await syncSelectedProvider()
    }

    // MARK: - Reading

    func refresh() async {
        do {
            settings = try await core.request("get_app_settings")
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// A settings write elsewhere lands here. The last read wins: an older one
    /// in flight would only put a stale screen back.
    private func scheduleRefresh() {
        refreshTask?.cancel()
        refreshTask = Task { [weak self] in
            guard let self, !Task.isCancelled else { return }
            await refresh()
            guard !Task.isCancelled else { return }
            await syncSelectedProvider()
        }
    }

    /// What the selected provider needs before its fields mean anything: its
    /// key state, and a model list once a lookup could succeed.
    private func syncSelectedProvider() async {
        guard let provider = settings?.selectedProvider else { return }
        let first = lastProbedProvider != provider.id
        lastProbedProvider = provider.id

        if provider.id == ProviderCatalog.appleIntelligenceId {
            if first { await checkAppleIntelligence() }
            return
        }
        if first { await refreshSecretState(provider.id) }
        await autoDiscoverIfNeeded()
    }

    // MARK: - The selected provider, as the screen reads it

    var selectedProviderId: String { settings?.selectedProviderId ?? ProviderCatalog.fallbackId }
    var selectedProvider: Provider? { settings?.selectedProvider }
    var providers: [Provider] { settings?.providers ?? [] }
    var enabled: Bool { settings?.enabled ?? false }
    var isAppleProvider: Bool { selectedProviderId == ProviderCatalog.appleIntelligenceId }
    var isCustomProvider: Bool { selectedProvider?.editableBaseUrl ?? false }
    var baseUrl: String { selectedProvider?.baseUrl ?? "" }
    var model: String { settings?.model(for: selectedProviderId) ?? "" }
    var prompts: [PostProcessPrompt] { settings?.prompts ?? [] }
    var selectedPromptId: String? { settings?.postProcessSelectedPromptId }

    /// The probe wins over the settings copy; both come from the same store and
    /// the probe is never older.
    func secretState(for providerId: String) -> SecretState? {
        llmSecrets[providerId] ?? settings?.secretState(for: providerId)
    }

    var selectedSecretState: SecretState? { secretState(for: selectedProviderId) }

    /// The store itself is out of reach, so a new key cannot be saved either.
    var isSecretUnavailable: Bool { selectedSecretState?.lastErrorKind == SecretFault("unavailable") }

    var endpoint: ProviderEndpoint {
        selectedProvider.map(ProviderEndpoint.of) ?? .invalid
    }

    /// The stored acknowledgement names this exact address, and grants it.
    var hasCurrentConsent: Bool {
        guard let address = endpoint.address,
              let consent = settings?.consent(for: selectedProviderId)
        else {
            return false
        }
        return consent.grants(address)
    }

    // MARK: - Model catalog

    private var currentScope: String? {
        guard let provider = selectedProvider else { return nil }
        return PostProcessCatalog.scope(providerId: provider.id, baseUrl: provider.baseUrl)
    }

    var catalogEntry: PostProcessCatalogEntry? {
        currentScope.flatMap { catalogs[$0] }
    }

    var allowsManualModelId: Bool { catalogEntry?.catalog.allowsManualModelId ?? true }

    var isDiscovering: Bool {
        currentScope.map { working.contains(Busy.catalog($0)) } ?? false
    }

    /// The menu: what the provider published, or the last list that loaded, and
    /// the saved selection merged in at the end so a failed refresh never drops
    /// the model this Mac is actually configured to use.
    var modelChoices: [PostProcessModelChoice] {
        var choices: [PostProcessModelChoice] = []
        var seen: Set<String> = []
        let entry = catalogEntry
        let ready = entry?.catalog.discovery == .ready

        for option in ready ? (entry?.catalog.models ?? []) : (entry?.cachedModels ?? []) {
            let id = option.id.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !id.isEmpty, seen.insert(id).inserted else { continue }
            choices.append(PostProcessModelChoice(id: id, source: ready ? .provider : .cached))
        }

        let saved = model.trimmingCharacters(in: .whitespacesAndNewlines)
        if !saved.isEmpty, !ready || !seen.contains(saved) {
            let marked = PostProcessModelChoice(id: saved, source: .saved)
            if let index = choices.firstIndex(where: { $0.id == saved }) {
                choices[index] = marked
            } else {
                choices.append(marked)
            }
        }
        return choices
    }

    /// The one thing a reader has to know about the last lookup. A finished
    /// load says nothing; the unreachable custom address is said once, above the
    /// field, so it is not repeated here.
    var modelStatus: [String] {
        if isDiscovering { return ["Loading models"] }
        guard let entry = catalogEntry else { return [] }

        var lines: [String] = []
        if !localEndpointUnreachable, let sentence = entry.catalog.discovery.sentence {
            lines.append(sentence)
        }

        let saved = model.trimmingCharacters(in: .whitespacesAndNewlines)
        let ready = entry.catalog.discovery == .ready
        if ready, !saved.isEmpty, !entry.catalog.models.contains(where: { $0.id == saved }) {
            lines.append("The saved selection is not in the latest provider list.")
        } else if !ready, !entry.cachedModels.isEmpty {
            lines.append("Using the last model list loaded in this window.")
        }
        return lines
    }

    /// A typed address that answers nothing is the reader's own server, and
    /// their problem to fix: it is said as a warning, not buried in the field.
    var localEndpointUnreachable: Bool {
        isCustomProvider && !isDiscovering && catalogEntry?.catalog.discovery == .unreachable
    }

    /// What makes a lookup worth repeating: the provider, the address, and
    /// every input the core checks before it asks.
    private func autoDiscoverySignature(for provider: Provider) -> String? {
        let address = provider.baseUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty else { return nil }

        let secret = secretState(for: provider.id)
        // A pinned provider has no key state until it has been probed; asking
        // then would only spend a round trip to be told the key is missing.
        guard provider.editableBaseUrl || secret != nil else { return nil }

        let consented = hasCurrentConsent ? "consented" : "needs-consent"
        let configured = secret.map { $0.configured ? "configured" : "missing" } ?? "unknown"
        let fault = secret?.lastErrorKind?.raw ?? "none"
        return [
            PostProcessCatalog.scope(providerId: provider.id, baseUrl: provider.baseUrl),
            consented,
            configured,
            fault,
        ].joined(separator: "\u{0}")
    }

    private func autoDiscoverIfNeeded() async {
        guard let provider = selectedProvider,
              provider.id != ProviderCatalog.appleIntelligenceId,
              let signature = autoDiscoverySignature(for: provider),
              signature != lastAutoDiscovery,
              !isDiscovering
        else {
            return
        }
        lastAutoDiscovery = signature
        await discover(provider)
    }

    /// Ask the provider for its models. A refusal is a state of the field, not
    /// an error banner: the sentence goes under the model row.
    private func discover(_ provider: Provider) async {
        let scope = PostProcessCatalog.scope(providerId: provider.id, baseUrl: provider.baseUrl)
        let key = Busy.catalog(scope)
        let manual = catalogs[scope]?.catalog.allowsManualModelId ?? true
        working.insert(key)
        defer { working.remove(key) }

        var catalog: PostProcessCatalog
        do {
            catalog = try await core.request(
                "discover_post_process_model_catalog",
                ["providerId": provider.id]
            )
            // A result for another provider answers a question nobody asked.
            if catalog.providerId != provider.id {
                catalog = .failed(
                    providerId: provider.id,
                    discovery: .invalidResponse,
                    allowsManualModelId: manual
                )
            }
        } catch is CoreError {
            // No answer at all: the address, the network, or the core.
            catalog = .failed(
                providerId: provider.id,
                discovery: .unreachable,
                allowsManualModelId: manual
            )
        } catch {
            // An answer nobody can read is not the same as silence.
            catalog = .failed(
                providerId: provider.id,
                discovery: .invalidResponse,
                allowsManualModelId: manual
            )
        }

        let cached = catalog.discovery == .ready ? catalog.models : (catalogs[scope]?.cachedModels ?? [])
        catalogs[scope] = PostProcessCatalogEntry(catalog: catalog, cachedModels: cached)
    }

    /// The reader asked for the list again, so the last signature no longer
    /// stands in the way.
    func refreshModels() async {
        guard let provider = selectedProvider, provider.id != ProviderCatalog.appleIntelligenceId else { return }
        await refreshSecretState(provider.id)
        guard let current = selectedProvider else { return }
        lastAutoDiscovery = autoDiscoverySignature(for: current)
        await discover(current)
    }

    /// A different address, or a different key, is a different answer.
    private func invalidateCatalog(_ providerId: String) {
        let prefix = providerId + "\u{0}"
        catalogs = catalogs.filter { !$0.key.hasPrefix(prefix) }
        lastAutoDiscovery = nil
    }

    // MARK: - Writing

    var isSavingProvider: Bool { working.contains(Busy.provider) }
    var isSavingEnabled: Bool { working.contains(Busy.enabled) }
    var isSavingBaseUrl: Bool { working.contains(Busy.baseUrl(selectedProviderId)) }
    var isSavingSecret: Bool { working.contains(Busy.secret(selectedProviderId)) }
    var isSavingModel: Bool { working.contains(Busy.model(selectedProviderId)) }
    var isSavingPrompt: Bool { working.contains(Busy.prompt) }
    var isAcceptingConsent: Bool { working.contains(Busy.consent) }

    func isBusy(_ provider: CloudSttProvider) -> Bool {
        working.contains(Busy.cloud(provider.accountId))
    }

    func isChecking(_ provider: CloudSttProvider) -> Bool {
        cloudChecking.contains(provider.accountId)
    }

    /// One write, then a read: none of these commands announce themselves, so
    /// the screen would otherwise show the value it sent rather than the value
    /// the core kept.
    @discardableResult
    private func write(_ key: String, _ method: String, _ params: [String: String]) async -> Bool {
        working.insert(key)
        defer { working.remove(key) }
        do {
            try await core.request(method, params)
            error = nil
            await refresh()
            return true
        } catch {
            self.error = error.localizedDescription
            return false
        }
    }

    func setEnabled(_ on: Bool) async {
        working.insert(Busy.enabled)
        defer { working.remove(Busy.enabled) }
        do {
            try await core.request("change_post_process_enabled_setting", ["enabled": on])
            error = nil
            await refresh()
        } catch {
            self.error = error.localizedDescription
        }
    }

    func select(_ providerId: String) async {
        guard providerId != selectedProviderId else { return }
        appleIntelligenceUnavailable = false
        endpointChanged = false
        consentError = nil

        guard await write(Busy.provider, "set_post_process_provider", ["providerId": providerId]) else { return }
        lastProbedProvider = nil
        await syncSelectedProvider()
    }

    /// The address the reader typed. The core drops the acknowledgement for the
    /// old address on its own; the saved model belonged to the old server, so
    /// it goes too.
    func commitBaseUrl(_ value: String) async {
        guard let provider = selectedProvider, provider.editableBaseUrl else { return }
        let next = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !next.isEmpty, next != provider.baseUrl.trimmingCharacters(in: .whitespacesAndNewlines) else { return }

        let saved = await write(
            Busy.baseUrl(provider.id),
            "change_post_process_base_url_setting",
            ["providerId": provider.id, "baseUrl": next]
        )
        guard saved else { return }

        endpointChanged = true
        invalidateCatalog(provider.id)
        if !model.isEmpty {
            await write(
                Busy.model(provider.id),
                "change_post_process_model_setting",
                ["providerId": provider.id, "model": ""]
            )
        }
        await autoDiscoverIfNeeded()
    }

    func selectModel(_ id: String) async {
        let providerId = selectedProviderId
        let next = id.trimmingCharacters(in: .whitespacesAndNewlines)
        guard next != model else { return }
        await write(
            Busy.model(providerId),
            "change_post_process_model_setting",
            ["providerId": providerId, "model": next]
        )
    }

    // MARK: - The provider key

    /// Read the store, not the settings: this is the answer the next request
    /// will actually get.
    func refreshSecretState(_ providerId: String) async {
        let key = Busy.secret(providerId)
        working.insert(key)
        defer { working.remove(key) }
        do {
            let state: SecretState = try await core.request(
                "get_provider_secret_state",
                ["kind": SecretKind.llm.rawValue, "providerId": providerId]
            )
            llmSecrets[providerId] = state
        } catch let failure as CoreError {
            error = failure.remote(as: SecretFault.self)?.sentence ?? failure.localizedDescription
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// The key itself never comes back out, so this is the only path that has
    /// it: it is handed to the store and dropped.
    func commitSecret(_ value: String) async {
        let providerId = selectedProviderId
        let secret = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !secret.isEmpty else { return }

        let key = Busy.secret(providerId)
        working.insert(key)
        defer { working.remove(key) }
        do {
            let state: SecretState = try await core.request(
                "set_provider_secret",
                ["kind": SecretKind.llm.rawValue, "providerId": providerId, "secret": secret]
            )
            llmSecrets[providerId] = state
            error = nil
            invalidateCatalog(providerId)
            await refresh()
            await autoDiscoverIfNeeded()
        } catch let failure as CoreError {
            error = failure.remote(as: SecretFault.self)?.sentence ?? failure.localizedDescription
        } catch {
            self.error = error.localizedDescription
        }
    }

    func deleteSecret() async {
        let providerId = selectedProviderId
        let key = Busy.secret(providerId)
        working.insert(key)
        defer { working.remove(key) }
        do {
            let state: SecretState = try await core.request(
                "delete_provider_secret",
                ["kind": SecretKind.llm.rawValue, "providerId": providerId]
            )
            llmSecrets[providerId] = state
            error = nil
            invalidateCatalog(providerId)
            await refresh()
        } catch let failure as CoreError {
            error = failure.remote(as: SecretFault.self)?.sentence ?? failure.localizedDescription
        } catch {
            self.error = error.localizedDescription
        }
    }

    // MARK: - Remote text transfer

    /// The acknowledgement is tied to the exact address, so the core rejects it
    /// when the address is local or malformed rather than recording a grant
    /// that would mean nothing.
    func acceptConsent() async -> Bool {
        guard let provider = selectedProvider else { return false }
        working.insert(Busy.consent)
        defer { working.remove(Busy.consent) }
        do {
            try await core.request("accept_post_process_provider_consent", ["providerId": provider.id])
            consentError = nil
            endpointChanged = false
            await refresh()
            await autoDiscoverIfNeeded()
            return true
        } catch let failure as CoreError {
            consentError = failure.remote(as: ProviderConsentFault.self)?.postProcessSentence
                ?? "Sona could not save this acknowledgement."
            return false
        } catch {
            consentError = "Sona could not save this acknowledgement."
            return false
        }
    }

    func clearConsentError() {
        consentError = nil
    }

    // MARK: - Apple Intelligence

    func checkAppleIntelligence() async {
        do {
            let available: Bool = try await core.request("check_apple_intelligence_available")
            appleIntelligenceUnavailable = !available
        } catch {
            appleIntelligenceUnavailable = false
            self.error = error.localizedDescription
        }
    }

    // MARK: - Prompt library

    func createPrompt(name: String, body: String) async -> Bool {
        await savePrompt("add_post_process_prompt", ["name": name, "prompt": body])
    }

    func updatePrompt(id: String, name: String, body: String) async -> Bool {
        await savePrompt("update_post_process_prompt", ["id": id, "name": name, "prompt": body])
    }

    private func savePrompt(_ method: String, _ params: [String: String]) async -> Bool {
        var trimmed = params
        trimmed["name"] = params["name"]?.trimmingCharacters(in: .whitespacesAndNewlines)
        trimmed["prompt"] = params["prompt"]?.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed["name"]?.isEmpty == false, trimmed["prompt"]?.isEmpty == false else { return false }

        working.insert(Busy.prompt)
        defer { working.remove(Busy.prompt) }
        do {
            try await core.request(method, trimmed)
            promptError = nil
            await refresh()
            return true
        } catch {
            promptError = error.localizedDescription
            return false
        }
    }

    /// The core keeps at least one prompt, and the screen says so rather than
    /// letting the reader find out from a refusal.
    func deletePrompt(_ id: String) async -> Bool {
        guard prompts.count > 1 else { return false }
        working.insert(Busy.prompt)
        defer { working.remove(Busy.prompt) }
        do {
            try await core.request("delete_post_process_prompt", ["id": id])
            promptError = nil
            await refresh()
            return true
        } catch {
            promptError = error.localizedDescription
            return false
        }
    }

    func usePrompt(_ id: String) async {
        working.insert(Busy.prompt)
        defer { working.remove(Busy.prompt) }
        do {
            try await core.request("set_post_process_selected_prompt", ["id": id])
            error = nil
            await refresh()
        } catch {
            self.error = error.localizedDescription
        }
    }

    func clearPromptError() {
        promptError = nil
    }

    // MARK: - Cloud transcription keys

    func refreshCloudSecretStates() async {
        for provider in CloudSttProvider.allCases {
            await refreshCloudSecretState(provider)
        }
    }

    func cloudSecretState(_ provider: CloudSttProvider) -> SecretState? {
        cloudSecrets[provider.accountId] ?? settings?.cloud(provider)?.secretState
    }

    func cloudError(_ provider: CloudSttProvider) -> String? {
        if let reported = cloudErrors[provider.accountId] { return reported }
        return cloudSecretState(provider)?.lastErrorKind?.sentence
    }

    func refreshCloudSecretState(_ provider: CloudSttProvider) async {
        cloudChecking.insert(provider.accountId)
        defer { cloudChecking.remove(provider.accountId) }
        await cloudCall(provider, "get_provider_secret_state", [
            "kind": SecretKind.stt.rawValue,
            "providerId": provider.accountId,
        ], refreshSettings: false)
    }

    func saveCloudSecret(_ provider: CloudSttProvider, secret: String) async {
        let key = secret.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !key.isEmpty else { return }
        await cloudCall(provider, "set_provider_secret", [
            "kind": SecretKind.stt.rawValue,
            "providerId": provider.accountId,
            "secret": key,
        ])
    }

    func removeCloudSecret(_ provider: CloudSttProvider) async {
        await cloudCall(provider, "delete_provider_secret", [
            "kind": SecretKind.stt.rawValue,
            "providerId": provider.accountId,
        ])
    }

    /// Verification spends a real request against the provider, so the core
    /// refuses it until the transfer is acknowledged.
    func verifyCloudSecret(_ provider: CloudSttProvider) async {
        await cloudCall(provider, "verify_stt_provider_secret", ["provider": provider.rawValue])
    }

    /// Every command in this row answers with the key's new state, which is the
    /// only thing the row shows.
    private func cloudCall(
        _ provider: CloudSttProvider,
        _ method: String,
        _ params: [String: String],
        refreshSettings: Bool = true
    ) async {
        let key = Busy.cloud(provider.accountId)
        working.insert(key)
        defer { working.remove(key) }
        cloudErrors[provider.accountId] = nil
        do {
            let state: SecretState = try await core.request(method, params)
            cloudSecrets[provider.accountId] = state
            if refreshSettings { await refresh() }
        } catch let failure as CoreError {
            cloudErrors[provider.accountId] = failure.remote(as: SecretFault.self)?.sentence
                ?? failure.localizedDescription
        } catch {
            cloudErrors[provider.accountId] = SecretFault.backend.sentence
        }
    }

    /// The three acknowledgements this build was written against, recorded in
    /// one go: the core stores them together and checks them together.
    func acceptCloudConsent(_ provider: CloudSttProvider) async {
        let key = Busy.cloud(provider.accountId)
        working.insert(key)
        defer { working.remove(key) }
        cloudErrors[provider.accountId] = nil
        do {
            try await core.request("accept_cloud_stt_provider_consent", ["provider": provider.rawValue])
            await refresh()
        } catch let failure as CoreError {
            cloudErrors[provider.accountId] = failure.remote(as: ProviderConsentFault.self)?.cloudSentence
                ?? failure.localizedDescription
        } catch {
            cloudErrors[provider.accountId] = "The acknowledgement could not be saved."
        }
    }
}

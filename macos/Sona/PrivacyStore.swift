import Foundation

/// Everything the Privacy screen knows: what Sona may read from other apps,
/// which routes off this Mac are live, how history is stored, and the two
/// one-time moves that brought data here — the legacy import and the identity
/// adoption.
///
/// Each read is independent, so one failure leaves the rest of the page
/// standing: a credential store that will not answer costs the egress chips,
/// not the ceiling.
@MainActor
@Observable
final class PrivacyStore {
    // Context capture.
    private(set) var contextCeiling: ContextPolicy = .none
    private(set) var urlCaptureEnabled = false
    private(set) var ceilingBusy = false
    private(set) var urlCaptureBusy = false
    private(set) var diagnostics: ContextDiagnostics?
    private(set) var diagnosticsBusy = false

    // What any other program on this Mac may do with the corpus.
    private(set) var externalQueryEnabled = false
    private(set) var externalMutationsEnabled = false
    private(set) var externalBusy = false

    // Egress.
    private(set) var cleanupRoute: EgressRoute = .checking
    private(set) var transcriptionRoute: EgressRoute = .checking
    private(set) var cloudSync: PrivacyCloudSyncStatus?
    private(set) var cloudSyncFailed = false

    // History at rest.
    private(set) var storage: StorageStatus?
    private(set) var storageFailure: String?

    // The legacy app's data.
    private(set) var upstream: UpstreamImportStatus?
    private(set) var upstreamBusy = false
    private(set) var upstreamImporting = false
    private(set) var upstreamProgress: UpstreamImportProgress?
    private(set) var upstreamResult: UpstreamImportResult?
    private(set) var upstreamFailure: String?
    /// Seeded once from the first status read, then only ever narrowed, so a
    /// refresh cannot silently tick a box the reader cleared.
    var upstreamSelection = UpstreamImportSelection()
    private var upstreamSelectionSeeded = false

    // The one-time adoption of a previous installation's identity.
    private(set) var identity: IdentityAdoptionReceipt?
    private(set) var identityFailure: String?
    private(set) var identityBusy = false
    private(set) var isPortable = false

    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.privacySettingsChanged) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadSettings() }
        }
        core.observe(CoreEvent.historyStorage) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadStorage() }
        }
        core.observe(CoreEvent.privacyCloudSyncChanged) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadCloudSync() }
        }
        core.observe(CoreEvent.upstreamImportProgressEvent) { [weak self] line in
            guard let progress: UpstreamImportProgress = try? Core.payload(line) else { return }
            self?.upstreamProgress = progress
        }
    }

    /// The first load. Every read is its own task: the page fills in as the
    /// core answers rather than waiting for the slowest one.
    func start() async {
        async let settings: Void = loadSettings()
        async let diagnostics: Void = loadDiagnostics()
        async let storage: Void = loadStorage()
        async let cloudSync: Void = loadCloudSync()
        async let upstream: Void = refreshUpstream()
        async let identity: Void = loadIdentity()
        async let portable: Void = loadPortable()
        _ = await (settings, diagnostics, storage, cloudSync, upstream, identity, portable)
    }

    // MARK: - Context capture

    func setContextCeiling(_ ceiling: ContextPolicy) async {
        guard ceiling != contextCeiling else { return }
        ceilingBusy = true
        defer { ceilingBusy = false }
        let previous = contextCeiling
        contextCeiling = ceiling
        do {
            try await core.request("change_context_policy_ceiling_setting", ["ceiling": ceiling.rawValue])
            error = nil
            await loadSettings()
            await loadDiagnostics()
        } catch {
            contextCeiling = previous
            self.error = "The context ceiling could not be saved. \(error.localizedDescription)"
        }
    }

    func setUrlCapture(_ enabled: Bool) async {
        urlCaptureBusy = true
        defer { urlCaptureBusy = false }
        let previous = urlCaptureEnabled
        urlCaptureEnabled = enabled
        do {
            try await core.request("change_context_url_capture_enabled_setting", ["enabled": enabled])
            error = nil
            await loadSettings()
            await loadDiagnostics()
        } catch {
            urlCaptureEnabled = previous
            self.error = "Browser URL capture could not be saved. \(error.localizedDescription)"
        }
    }

    func refreshDiagnostics() async {
        await loadDiagnostics()
    }

    // MARK: - External access

    func setExternalQuery(_ enabled: Bool) async {
        await setExternal("change_external_query_enabled_setting", enabled, keyPath: \.externalQueryEnabled)
    }

    func setExternalMutations(_ enabled: Bool) async {
        await setExternal("change_external_mutations_enabled_setting", enabled, keyPath: \.externalMutationsEnabled)
    }

    private func setExternal(
        _ method: String, _ enabled: Bool, keyPath: ReferenceWritableKeyPath<PrivacyStore, Bool>
    ) async {
        externalBusy = true
        defer { externalBusy = false }
        let previous = self[keyPath: keyPath]
        self[keyPath: keyPath] = enabled
        do {
            try await core.request(method, ["enabled": enabled])
            error = nil
            await loadSettings()
        } catch {
            self[keyPath: keyPath] = previous
            self.error = "That permission could not be saved. \(error.localizedDescription)"
        }
    }

    // MARK: - Egress

    /// A failed secret-state read is transient — the credential store can be
    /// locked — so the row offers this rather than a dead end.
    func retryEgress() async {
        cleanupRoute = .checking
        transcriptionRoute = .checking
        guard let settings = try? await appSettings() else {
            cleanupRoute = .failed
            transcriptionRoute = .failed
            return
        }
        await loadRoutes(settings)
    }

    func reloadCloudSync() async {
        await loadCloudSync()
    }

    // MARK: - History storage

    func reloadStorage() async {
        await loadStorage()
    }

    // MARK: - Upstream import

    func refreshUpstream() async {
        upstreamBusy = true
        defer { upstreamBusy = false }
        do {
            let status: UpstreamImportStatus = try await core.request("get_upstream_import_status")
            upstream = status
            upstreamFailure = nil
            narrowSelection(to: status)
        } catch let failure as CoreError {
            upstream = nil
            // A Mac with no legacy app is the ordinary case, not a failure.
            let code = failure.remote(as: UpstreamImportFailure.self)
            upstreamFailure = code == .sourceUnavailable
                ? nil
                : (code?.sentence ?? "The legacy app's data could not be inspected.")
        } catch {
            upstream = nil
            upstreamFailure = "The legacy app's data could not be inspected."
        }
    }

    /// Ticking history off unticks recordings: recordings are imported only
    /// with history, and a hidden tick is a promise the import will not keep.
    func setUpstreamHistory(_ history: Bool) {
        upstreamSelection.history = history
        if !history {
            upstreamSelection.recordings = false
        }
    }

    func setUpstreamSettings(_ settings: Bool) {
        upstreamSelection.settings = settings
    }

    func setUpstreamRecordings(_ recordings: Bool) {
        upstreamSelection.recordings = recordings
    }

    /// The source holds something worth importing at all.
    var upstreamHasData: Bool {
        upstream?.settingsAvailable == true || (upstream?.historyEntries ?? 0) > 0
    }

    /// What is ticked can actually be imported.
    var upstreamSelectionValid: Bool {
        (upstreamSelection.settings && upstream?.settingsAvailable == true)
            || (upstreamSelection.history && (upstream?.historyEntries ?? 0) > 0)
    }

    var upstreamImportAvailable: Bool {
        upstream?.available == true && upstream?.appState == .closed
    }

    func startUpstreamImport() async {
        guard !upstreamImporting, let status = upstream, status.appState == .closed else { return }
        guard upstreamSelectionValid else {
            upstreamFailure = UpstreamImportFailure.invalidSelection.sentence
            return
        }
        upstreamImporting = true
        upstreamFailure = nil
        upstreamProgress = nil
        upstreamResult = nil
        defer { upstreamImporting = false }
        do {
            let result: UpstreamImportResult = try await core.request(
                "import_legacy_app", ["selection": try JSONValue(upstreamSelection)])
            upstreamResult = result
            error = nil
            await loadSettings()
            await refreshUpstream()
        } catch let failure as CoreError {
            upstreamFailure = failure.remote(as: UpstreamImportFailure.self)?.sentence
                ?? UpstreamImportFailure.internalFailure.sentence
        } catch {
            upstreamFailure = UpstreamImportFailure.internalFailure.sentence
        }
    }

    /// Put the settings this Mac had before the import back, from the backup
    /// the import wrote. History and recordings stay: they were added to, not
    /// replaced.
    func revertUpstreamSettings() async {
        guard !upstreamImporting else { return }
        upstreamImporting = true
        upstreamFailure = nil
        upstreamResult = nil
        defer { upstreamImporting = false }
        do {
            try await core.request("revert_upstream_import_settings")
            error = nil
            await loadSettings()
            await refreshUpstream()
        } catch let failure as CoreError {
            upstreamFailure = failure.remote(as: UpstreamImportFailure.self)?.sentence
                ?? UpstreamImportFailure.internalFailure.sentence
        } catch {
            upstreamFailure = UpstreamImportFailure.internalFailure.sentence
        }
    }

    // MARK: - Identity adoption

    func refreshIdentity() async {
        await loadIdentity()
    }

    /// Move the adopted data folder back where it came from. Only a completed
    /// adoption can be undone, and only while the legacy app is closed.
    func revertIdentity() async {
        guard !identityBusy else { return }
        identityBusy = true
        identityFailure = nil
        defer { identityBusy = false }
        do {
            try await core.request("revert_identity_adoption")
            error = nil
            await loadIdentity()
        } catch let failure as CoreError {
            identityFailure = failure.remote(as: IdentityAdoptionFailure.self)?.sentence
                ?? failure.localizedDescription
        } catch {
            identityFailure = error.localizedDescription
        }
    }

    // MARK: - Reads

    private func appSettings() async throws -> PrivacySettings {
        try await core.request("get_app_settings")
    }

    private func loadSettings() async {
        do {
            let settings = try await appSettings()
            contextCeiling = settings.contextPolicyCeiling ?? .none
            urlCaptureEnabled = settings.contextUrlCaptureEnabled ?? false
            externalQueryEnabled = settings.externalQueryEnabled ?? false
            externalMutationsEnabled = settings.externalMutationsEnabled ?? false
            error = nil
            await loadRoutes(settings)
        } catch {
            self.error = error.localizedDescription
            cleanupRoute = .failed
            transcriptionRoute = .failed
        }
    }

    private func loadDiagnostics() async {
        diagnosticsBusy = true
        defer { diagnosticsBusy = false }
        do {
            diagnostics = try await core.request("get_context_diagnostics")
        } catch {
            self.error = "Diagnostics could not be refreshed. \(error.localizedDescription)"
        }
    }

    private func loadStorage() async {
        do {
            storage = try await core.request("history_storage_status")
            storageFailure = nil
        } catch {
            storage = nil
            storageFailure = "Sona could not read how history is stored. \(error.localizedDescription)"
        }
    }

    private func loadCloudSync() async {
        do {
            cloudSync = try await core.request("cloud_sync_service_status")
            cloudSyncFailed = false
        } catch {
            cloudSync = nil
            cloudSyncFailed = true
        }
    }

    private func loadIdentity() async {
        do {
            identity = try await core.request("get_identity_adoption_status")
            identityFailure = nil
        } catch let failure as CoreError {
            identity = nil
            identityFailure = failure.remote(as: IdentityAdoptionFailure.self)?.sentence
                ?? failure.localizedDescription
        } catch {
            identity = nil
            identityFailure = error.localizedDescription
        }
    }

    private func loadPortable() async {
        isPortable = (try? await core.request("is_portable")) ?? false
    }

    /// Which routes off this Mac are live, read from the credential store
    /// rather than inferred from settings: a provider is a route only once a
    /// key for it exists.
    private func loadRoutes(_ settings: PrivacySettings) async {
        await loadCleanupRoute(settings)
        await loadTranscriptionRoute(settings)
    }

    private func loadCleanupRoute(_ settings: PrivacySettings) async {
        var destinations = Set<String>()
        for mode in settings.modes ?? [] where mode.llm.enabled {
            // A mode with no provider of its own sends to the global one, so
            // the route it opens is the global provider's.
            destinations.insert(mode.llm.providerId ?? settings.postProcessProviderId ?? "")
        }
        let candidates = (settings.postProcessProviders ?? []).filter { provider in
            guard destinations.contains(provider.id), provider.id != "apple_intelligence" else { return false }
            guard let host = URL(string: provider.baseUrl)?.host?.lowercased() else { return true }
            return host != "localhost" && host != "127.0.0.1" && host != "::1"
        }
        guard !candidates.isEmpty else {
            cleanupRoute = .thisMac
            return
        }
        var configured: [String] = []
        for provider in candidates {
            // A read that fails counts as no key: the credential store can be
            // locked, and a route is only a route once a key is proven there.
            if await secretConfigured(kind: "llm", id: provider.id) {
                configured.append(provider.label)
            }
        }
        cleanupRoute = configured.isEmpty ? .thisMac : .providers(configured)
    }

    private func loadTranscriptionRoute(_ settings: PrivacySettings) async {
        var live: [String] = []
        var refused = false
        for provider in EgressConsent.cloudSttProviders {
            let state = settings.cloudSttProviders?.first { $0.provider == provider.provider }
            guard EgressConsent.isCurrent(state) else { continue }
            do {
                let secret: EgressSecretState = try await core.request(
                    "get_provider_secret_state", ["kind": "stt", "providerId": provider.account])
                if secret.configured {
                    live.append(provider.label)
                }
            } catch {
                refused = true
            }
        }
        // A route whose state could not be read shows no fact at all: a chip
        // reading "this Mac" would be a guess, and this is the one page that
        // cannot guess.
        transcriptionRoute = refused ? .failed : (live.isEmpty ? .thisMac : .providers(live))
    }

    private func secretConfigured(kind: String, id: String) async -> Bool {
        let state: EgressSecretState? = try? await core.request(
            "get_provider_secret_state", ["kind": kind, "providerId": id])
        return state?.configured ?? false
    }

    private func narrowSelection(to status: UpstreamImportStatus) {
        guard upstreamSelectionSeeded else {
            upstreamSelectionSeeded = true
            upstreamSelection = UpstreamImportSelection(
                settings: status.settingsAvailable && !status.settingsImported,
                history: status.historyEntries > 0,
                recordings: false)
            return
        }
        upstreamSelection = UpstreamImportSelection(
            settings: upstreamSelection.settings && status.settingsAvailable,
            history: upstreamSelection.history && status.historyEntries > 0,
            recordings: upstreamSelection.recordings
                && status.historyEntries > 0
                && status.recordingFiles > 0)
    }
}

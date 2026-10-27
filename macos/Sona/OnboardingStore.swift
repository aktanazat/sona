import Foundation

enum OnboardingStep: Equatable {
    case probing
    case languages
    case model
    case permissions
    case practice
    case done
}

/// One model as the first run needs it. A narrower read of the core's
/// `ModelInfo` than the catalog screen's: the fields a reader picking their
/// first model acts on, plus the two the arrangement needs.
struct OnboardingModel: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
    let description: String
    let sizeMb: UInt64
    let isDownloaded: Bool
    let isDownloading: Bool
    let isRecommended: Bool
    let source: OnboardingModelSource
    let supportedLanguages: [String]
    let supportsLanguageDetection: Bool

    func supports(_ languages: [String]) -> Bool {
        if languages.isEmpty {
            return supportsLanguageDetection && supportedLanguages.count > 1
        }
        return languages.allSatisfy { LanguageCatalog.supports(supportedLanguages, $0) }
    }

    /// "620 MB", "3 GB", "3.1 GB", "12 GB": whole gigabytes from ten up, and
    /// never a trailing zero.
    var sizeText: String {
        guard sizeMb > 0 else { return "Unknown size" }
        guard sizeMb >= 1024 else { return "\(sizeMb) MB" }
        let gigabytes = Double(sizeMb) / 1024
        let tenths = gigabytes >= 10 ? gigabytes.rounded() : (gigabytes * 10).rounded() / 10
        return tenths == tenths.rounded()
            ? "\(Int(tenths)) GB"
            : String(format: "%.1f GB", tenths)
    }
}

/// Where a model comes from. The core's `ModelSource` is an externally tagged
/// enum — `"Local"`, `{"Url": {...}}`, `{"HuggingFace": {...}}` — so this reads
/// the tag and keeps only what the first run decides with.
enum OnboardingModelSource: Decodable, Equatable {
    case local
    case url
    case huggingFace

    init(from decoder: Decoder) throws {
        switch try JSONValue(from: decoder) {
        case .string:
            self = .local
        case .object(let fields):
            self = fields.keys.contains("Url") ? .url : .huggingFace
        default:
            self = .local
        }
    }

    /// A blob `.bin`/ONNX model: still runnable, never offered as a download
    /// now that the catalog GGUFs supersede it.
    var isLegacy: Bool {
        self == .url
    }
}

/// What is happening to one model right now.
enum OnboardingModelWork: Equatable {
    /// On disk, ready to be made the active model.
    case available
    /// Not on disk yet.
    case downloadable
    case downloading
    case verifying
    case extracting
    case cancelling
    /// The core is being told to make it the active model.
    case switching

    var isBusy: Bool {
        switch self {
        case .available, .downloadable: return false
        case .downloading, .verifying, .extracting, .switching, .cancelling: return true
        }
    }
}

/// `model-download-failed`: the one model event that carries a reason.
struct OnboardingDownloadFailure: Decodable {
    let modelId: String
    let error: String
}

enum OnboardingPracticeState: Equatable {
    case idle
    case starting
    case listening
    case processing
    case delivered
    case cancelling
    case cancellationFailed

    var isBusy: Bool {
        switch self {
        case .starting, .listening, .processing, .cancelling, .cancellationFailed: true
        case .idle, .delivered: false
        }
    }
}

private struct OnboardingPracticeTranscript: Decodable {
    let sessionId: String
    let text: String
}

private struct OnboardingPracticeExpired: Decodable {
    let sessionId: String
    let error: String
}

@MainActor
@Observable
final class OnboardingStore {
    private(set) var step: OnboardingStep = .probing
    private(set) var models: [OnboardingModel] = []
    private(set) var selectedLanguages: [String] = []
    private(set) var savingLanguages = false
    private(set) var chosenId: String?
    private(set) var error: String?
    private(set) var practiceState: OnboardingPracticeState = .idle
    private(set) var finishing = false
    var practiceText = ""
    let permissions: PermissionsStore

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var returning = false
    @ObservationIgnored private var activeModelId = ""
    @ObservationIgnored private var selection: UInt64 = 0
    @ObservationIgnored private var cancellingId: String?
    @ObservationIgnored private var practiceId: String?
    @ObservationIgnored private var practiceLimit: Task<Void, Never>?
    private var work: [String: OnboardingModelWork] = [:]
    private var progress: [String: Double] = [:]

    init(core: Core) {
        self.core = core
        permissions = PermissionsStore(core: core)
        core.observe(CoreEvent.downloadProgress) { [weak self] line in
            guard let self, let progress: DownloadProgress = try? Core.payload(line),
                  self.work[progress.modelId] != nil else { return }
            self.progress[progress.modelId] = progress.total > 0
                ? Double(progress.downloaded) / Double(progress.total) : 0
        }
        core.observe(CoreEvent.onboardingVerificationStarted) { [weak self] line in
            self?.mark(line, as: .verifying)
        }
        core.observe(CoreEvent.onboardingVerificationCompleted) { [weak self] line in
            self?.mark(line, as: .downloading)
        }
        core.observe(CoreEvent.onboardingExtractionStarted) { [weak self] line in
            self?.mark(line, as: .extracting)
        }
        core.observe(CoreEvent.onboardingExtractionCompleted) { [weak self] line in
            self?.mark(line, as: .downloading)
        }
        core.observe(CoreEvent.downloadComplete) { [weak self] line in
            guard let self, let id: String = try? Core.payload(line) else { return }
            self.progress[id] = nil
            Task { await self.loadModels() }
        }
        for event in [CoreEvent.downloadFailed, CoreEvent.extractionFailed] {
            core.observe(event) { [weak self] line in
                guard let self, let failure: OnboardingDownloadFailure = try? Core.payload(line),
                      self.chosenId == failure.modelId else { return }
                self.error = failure.error
            }
        }
        core.observe(CoreEvent.downloadCancelled) { [weak self] line in
            guard let self, let id: String = try? Core.payload(line), self.chosenId != id else { return }
            self.work[id] = nil
            self.progress[id] = nil
        }
        core.observe(CoreEvent.modelsUpdated) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadModels() }
        }
        core.observe("onboarding-practice-expired") { [weak self] line in
            guard let self, let expired: OnboardingPracticeExpired = try? Core.payload(line),
                  self.practiceId == expired.sessionId else { return }
            self.practiceLimit?.cancel()
            self.practiceId = nil
            self.practiceState = .idle
            self.error = expired.error
        }
    }

    func start() async {
        permissions.onAllGranted = { [weak self] in self?.permissionsSettled() }
        do {
            let settings: OnboardingSettings = try await core.request("get_app_settings")
            returning = settings.onboardingCompleted
            activeModelId = settings.selectedModel
            selectedLanguages = settings.selectedLanguage == "auto"
                ? settings.dictationLanguages : [settings.selectedLanguage]
            error = nil
        } catch {
            self.error = error.localizedDescription
            return
        }
        await permissions.start()
        guard step == .probing else { return }
        if returning {
            if permissions.allGranted {
                finish()
            } else {
                step = .permissions
                permissions.recheck()
            }
        } else {
            step = .languages
        }
    }

    var languageSummary: String {
        selectedLanguages.isEmpty ? "Auto detect" : selectedLanguages.map(LanguageCatalog.name).joined(separator: ", ")
    }

    func toggleLanguage(_ code: String) {
        guard !savingLanguages else { return }
        selectedLanguages = LanguageCatalog.toggling(code, in: selectedLanguages)
    }

    func useAutomaticLanguage() {
        guard !savingLanguages else { return }
        selectedLanguages = []
    }

    func saveLanguages() {
        guard !savingLanguages else { return }
        savingLanguages = true
        error = nil
        Task {
            defer { savingLanguages = false }
            do {
                try await core.request("change_dictation_languages_setting", [
                    "languages": JSONValue.array(selectedLanguages.map(JSONValue.string)),
                ])
                step = .model
                await loadModels()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    func changeLanguages() {
        guard !isBusy else { return }
        error = nil
        step = .languages
    }

    private var compatibleModels: [OnboardingModel] {
        models.filter { $0.supports(selectedLanguages) }
    }

    private var onDiskModels: [OnboardingModel] {
        let installed = compatibleModels.filter(\.isDownloaded)
        return installed.filter { $0.id == activeModelId } + installed.filter { $0.id != activeModelId }
    }

    private var rankedDownloads: [OnboardingModel] {
        let downloadable = compatibleModels.filter { !$0.isDownloaded && !$0.source.isLegacy }
        return downloadable.filter(\.isRecommended) + downloadable.filter { !$0.isRecommended }
    }

    var featured: OnboardingModel? { onDiskModels.first ?? rankedDownloads.first }
    var otherOnDisk: [OnboardingModel] { onDiskModels.filter { $0.id != featured?.id } }
    var otherDownloads: [OnboardingModel] { rankedDownloads.filter { $0.id != featured?.id } }
    var otherCount: Int { otherOnDisk.count + otherDownloads.count }
    var isBusy: Bool { chosenId != nil || cancellingId != nil }

    func work(for id: String) -> OnboardingModelWork {
        work[id] ?? (models.first(where: { $0.id == id })?.isDownloaded == true ? .available : .downloadable)
    }

    func progress(for id: String) -> Double? {
        work(for: id) == .downloading ? progress[id] : nil
    }

    func loadModels() async {
        do {
            let list: [OnboardingModel] = try await core.request("get_available_models")
            models = list
            for model in list {
                if model.isDownloading, work[model.id] == nil {
                    work[model.id] = .downloading
                } else if !model.isDownloading, work[model.id] == .downloading, progress[model.id] == nil {
                    work[model.id] = nil
                }
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    func choose(_ model: OnboardingModel) {
        guard !isBusy, model.supports(selectedLanguages) else { return }
        selection += 1
        let token = selection
        chosenId = model.id
        error = nil
        work[model.id] = model.isDownloaded ? .switching : .downloading
        Task {
            do {
                if !model.isDownloaded {
                    try await core.request("download_model", ["modelId": model.id])
                }
                guard selection == token, chosenId == model.id else { return }
                work[model.id] = .switching
                try await core.request("set_active_model", ["modelId": model.id])
                guard selection == token else { return }
                activeModelId = model.id
                clear(model.id)
                error = nil
                if permissions.allGranted {
                    step = .practice
                } else {
                    step = .permissions
                    permissions.recheck()
                }
            } catch {
                guard selection == token else { return }
                self.error = error.localizedDescription
                clear(model.id)
            }
        }
    }

    func cancel(_ id: String) {
        guard cancellingId == nil else { return }
        selection += 1
        chosenId = nil
        cancellingId = id
        work[id] = .cancelling
        Task {
            defer { cancellingId = nil }
            do {
                try await core.request("cancel_download", ["modelId": id])
                clear(id)
                error = nil
            } catch {
                work[id] = nil
                self.error = error.localizedDescription
            }
        }
    }

    func startPractice() {
        guard step == .practice, !practiceState.isBusy, !finishing else { return }
        let id = UUID().uuidString
        practiceId = id
        practiceText = ""
        practiceState = .starting
        error = nil
        Task {
            do {
                try await core.request("start_onboarding_practice", ["sessionId": id])
                guard practiceId == id, practiceState == .starting else { return }
                practiceState = .listening
                practiceLimit = Task {
                    try? await Task.sleep(for: .seconds(30))
                    guard !Task.isCancelled, practiceId == id, practiceState == .listening else { return }
                    stopPractice()
                }
            } catch {
                guard practiceId == id, practiceState == .starting else { return }
                practiceId = nil
                practiceState = .idle
                self.error = error.localizedDescription
            }
        }
    }

    func stopPractice() {
        guard let id = practiceId, practiceState == .listening else { return }
        practiceLimit?.cancel()
        practiceLimit = nil
        practiceState = .processing
        Task {
            do {
                let result: OnboardingPracticeTranscript = try await core.request(
                    "stop_onboarding_practice", ["sessionId": id]
                )
                guard practiceId == id, practiceState == .processing, result.sessionId == id else { return }
                practiceText = result.text
                practiceState = .delivered
                error = nil
            } catch {
                guard practiceId == id, practiceState == .processing else { return }
                practiceId = nil
                practiceState = .idle
                self.error = error.localizedDescription
            }
        }
    }

    func cancelPractice() {
        guard let id = practiceId, practiceState != .cancelling, !finishing else { return }
        practiceLimit?.cancel()
        practiceLimit = nil
        practiceState = .cancelling
        Task {
            do {
                try await core.request("cancel_onboarding_practice", ["sessionId": id])
                guard practiceId == id else { return }
                practiceId = nil
                practiceState = .idle
                practiceText = ""
                error = nil
            } catch {
                guard practiceId == id else { return }
                practiceState = .cancellationFailed
                self.error = error.localizedDescription
            }
        }
    }

    func backToModels() {
        guard !practiceState.isBusy, !finishing else { return }
        if let id = practiceId {
            practiceState = .cancelling
            Task {
                do {
                    try await core.request("cancel_onboarding_practice", ["sessionId": id])
                    practiceId = nil
                    practiceState = .idle
                    practiceText = ""
                    error = nil
                    step = .model
                    await loadModels()
                } catch {
                    practiceState = .cancellationFailed
                    self.error = error.localizedDescription
                }
            }
        } else {
            step = .model
        }
    }

    var canFinishPractice: Bool {
        practiceState == .delivered && !practiceText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !finishing
    }

    func completePractice() {
        guard let id = practiceId, canFinishPractice else { return }
        finishing = true
        Task {
            defer { finishing = false }
            do {
                try await core.request("complete_onboarding_practice", ["sessionId": id])
                error = nil
                finish()
            } catch {
                self.error = error.localizedDescription
            }
        }
    }

    func permissionsSettled() {
        guard step == .permissions, permissions.allGranted else { return }
        if returning {
            finish()
        } else {
            step = .practice
        }
    }

    private func finish() {
        permissions.initializeInput()
        step = .done
    }

    private func mark(_ line: Data, as state: OnboardingModelWork) {
        guard let id: String = try? Core.payload(line), work[id] != nil else { return }
        work[id] = state
    }

    private func clear(_ id: String) {
        work[id] = nil
        progress[id] = nil
        if chosenId == id { chosenId = nil }
    }
}

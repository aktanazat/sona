import Foundation

/// Which of the first run's two questions is on screen.
enum OnboardingStep: Equatable {
    /// The flags and the permissions are still being read.
    case probing
    case permissions
    case model
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
    /// The core is being told to make it the active model.
    case switching

    var isBusy: Bool {
        switch self {
        case .available, .downloadable: return false
        case .downloading, .verifying, .extracting, .switching: return true
        }
    }
}

/// `model-download-failed`: the one model event that carries a reason.
struct OnboardingDownloadFailure: Decodable {
    let modelId: String
    let error: String
}

/// The first run: the permissions macOS asks for, then one model on this Mac.
///
/// The order is the web app's, and so is what each step means. Permissions come
/// first because a model download is long and a reader who granted nothing has
/// an app that cannot hear or type at the end of it. The model step is what
/// finishes onboarding: the core writes `onboarding_completed` inside
/// `set_active_model` (src-tauri/src/commands/models.rs:122), so a first run
/// that picked a model is over, and one that did not will come back.
///
/// A returning user who has lost a permission gets the permission step and
/// nothing else: they already have a model.
@MainActor
@Observable
final class OnboardingStore {
    private(set) var step: OnboardingStep = .probing
    private(set) var models: [OnboardingModel] = []
    /// The model the reader chose, while it is being fetched or switched to.
    private(set) var chosenId: String?
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    /// The permission state this flow's first step shows. Shared rather than
    /// rebuilt, so the banner outside the flow reads the same values.
    let permissions: PermissionsStore

    @ObservationIgnored private let core: Core
    /// Per model work, keyed by model id, as the core's events report it.
    private var work: [String: OnboardingModelWork] = [:]
    /// Download progress as a fraction, for the one model being fetched.
    private var progress: [String: Double] = [:]
    /// Downloads this screen cancelled: their command fails on purpose, so the
    /// failure is not worth showing.
    @ObservationIgnored private var cancelled: Set<String> = []
    /// True when this Mac had already finished onboarding, so the model step
    /// is not this session's business.
    @ObservationIgnored private var returning = false

    init(core: Core) {
        self.core = core
        permissions = PermissionsStore(core: core)
        core.observe(CoreEvent.downloadProgress) { [weak self] line in
            guard let self, let progress: DownloadProgress = try? Core.payload(line) else { return }
            guard self.work[progress.modelId] != nil else { return }
            self.progress[progress.modelId] = progress.total > 0
                ? Double(progress.downloaded) / Double(progress.total)
                : 0
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
        core.observe(CoreEvent.downloadFailed) { [weak self] line in
            guard let self, let failure: OnboardingDownloadFailure = try? Core.payload(line) else { return }
            self.clear(failure.modelId)
            // A download this screen cancelled fails on purpose.
            if self.cancelled.remove(failure.modelId) == nil {
                self.error = failure.error
            }
        }
        core.observe(CoreEvent.downloadCancelled) { [weak self] line in
            guard let self, let id: String = try? Core.payload(line) else { return }
            self.cancelled.remove(id)
            self.clear(id)
        }
        core.observe(CoreEvent.modelsUpdated) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadModels() }
        }
    }

    /// Reads the onboarding flag, then the permissions, and lands on the step
    /// this launch actually needs. Called once by the integrator after the core
    /// is up; nothing else decides whether the flow shows.
    func start() async {
        permissions.onAllGranted = { [weak self] in self?.permissionsSettled() }
        do {
            let settings: OnboardingSettings = try await core.request("get_app_settings")
            returning = settings.onboardingCompleted
            error = nil
        } catch {
            // A first run that cannot read its own flag is treated as a first
            // run: the worst case is a reader confirming a model they have.
            returning = false
            self.error = error.localizedDescription
        }
        await permissions.start()
        guard step == .probing else { return }
        if permissions.allGranted {
            permissionsSettled()
            return
        }
        if returning {
            permissions.revealWindow()
        }
        step = .permissions
        permissions.recheck()
    }

    /// The model catalog, and the arrangement the first run reads it in.
    ///
    /// One model on the page, every other model behind one summary. The catalog
    /// arrives rank sorted, so the first recommended download is the editorial
    /// pick — but a model already on this Mac beats it, because choosing that
    /// one costs no download at all.
    var featured: OnboardingModel? {
        onDiskModels.first ?? rankedDownloads.first
    }

    /// Models already on this Mac, in catalog order.
    private var onDiskModels: [OnboardingModel] {
        models.filter(\.isDownloaded)
    }

    /// What is worth offering as a download, recommended first. Legacy sources
    /// are never offered: they stay runnable, and they still show up above once
    /// they are on disk.
    private var rankedDownloads: [OnboardingModel] {
        let downloadable = models.filter { !$0.isDownloaded && !$0.source.isLegacy }
        return downloadable.filter(\.isRecommended) + downloadable.filter { !$0.isRecommended }
    }

    var otherOnDisk: [OnboardingModel] {
        let pick = featured?.id
        return onDiskModels.filter { $0.id != pick }
    }

    var otherDownloads: [OnboardingModel] {
        let pick = featured?.id
        return rankedDownloads.filter { $0.id != pick }
    }

    var otherCount: Int {
        otherOnDisk.count + otherDownloads.count
    }

    /// True once the reader has chosen: every other card goes quiet.
    var isBusy: Bool {
        chosenId != nil
    }

    func work(for id: String) -> OnboardingModelWork {
        if let state = work[id] {
            return state
        }
        return models.first(where: { $0.id == id })?.isDownloaded == true ? .available : .downloadable
    }

    /// The fraction of the download that has arrived, or nil while the work has
    /// no length to measure (verifying, unpacking, switching).
    func progress(for id: String) -> Double? {
        work(for: id) == .downloading ? progress[id] : nil
    }

    func loadModels() async {
        do {
            let list: [OnboardingModel] = try await core.request("get_available_models")
            models = list
            error = nil
            // The core is the authority on what is downloading: a download
            // resumed from a previous session has no state here yet, and one
            // this screen believed in may have ended while it was away.
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

    /// The one gesture on the model step: take this model. A model on disk is
    /// made active; a model that is not is fetched first and then made active.
    func choose(_ model: OnboardingModel) {
        guard chosenId == nil else { return }
        chosenId = model.id
        if model.isDownloaded {
            work[model.id] = .switching
            Task { await activate(model.id) }
        } else {
            work[model.id] = .downloading
            Task { await download(model.id) }
        }
    }

    func cancel(_ id: String) {
        cancelled.insert(id)
        Task {
            do {
                try await core.request("cancel_download", ["modelId": id])
                clear(id)
                error = nil
            } catch {
                cancelled.remove(id)
                self.error = error.localizedDescription
            }
        }
    }

    /// Skips nothing and grants nothing: the reader is on the permission step
    /// with both permissions held, which only happens for the moment between
    /// the last grant and the next screen.
    private func permissionsSettled() {
        guard step == .probing || step == .permissions else { return }
        if returning {
            finish()
            return
        }
        step = .model
        Task { await loadModels() }
    }

    /// `download_model` returns when the bytes are on disk, verified and
    /// unpacked — the events only narrate it — so the model is ready to be
    /// activated the moment this resolves.
    private func download(_ id: String) async {
        do {
            try await core.request("download_model", ["modelId": id])
            error = nil
            work[id] = .switching
            await activate(id)
        } catch {
            if cancelled.remove(id) == nil {
                self.error = error.localizedDescription
            }
            clear(id)
        }
    }

    private func activate(_ id: String) async {
        do {
            try await core.request("set_active_model", ["modelId": id])
            error = nil
            clear(id)
            finish()
        } catch {
            self.error = "Couldn't select the model. \(error.localizedDescription)"
            clear(id)
        }
    }

    /// Onboarding is over. Typing and the global shortcuts are started here for
    /// the reader who granted accessibility before this launch: the grant
    /// transition never happened, so nothing else would have started them.
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
        if chosenId == id {
            chosenId = nil
        }
    }
}

import AppKit
import Foundation
import Observation
import SwiftUI

/// The four places in the sidebar, in the order it shows them.
enum Place: Int, CaseIterable, Identifiable {
    case capture, library, meetings, people

    var id: Int { rawValue }

    var title: String {
        switch self {
        case .capture: "Capture"
        case .library: "Library"
        case .meetings: "Meetings"
        case .people: "People"
        }
    }

    var icon: String {
        switch self {
        case .capture: "mic"
        case .library: "books.vertical"
        case .meetings: "video"
        case .people: "person.2"
        }
    }
}

/// The tabs across the top of Settings, in the order the strip shows them.
enum SettingsPlace: Int, CaseIterable, Identifiable {
    case essentials, dictation, models, modes, vocabulary, prompts, workflows, meetings
    case agents, sync, privacy, importing, documents, about, debug

    var id: Int { rawValue }

    var title: String {
        switch self {
        case .essentials: "Essentials"
        case .dictation: "Dictation"
        case .models: "Models"
        case .modes: "Modes"
        case .vocabulary: "Vocabulary"
        case .prompts: "Prompts"
        case .workflows: "Workflows"
        case .meetings: "Meetings"
        case .agents: "Agents"
        case .sync: "Sync"
        case .privacy: "Privacy"
        case .importing: "Import"
        case .documents: "Documents"
        case .about: "About"
        case .debug: "Debug"
        }
    }
}

/// What floats over the main window. One at a time.
enum Sheet: Identifiable {
    case chat
    case recorder
    case whatsNew

    var id: Self { self }
}

/// `hud_pill_state`: the mode the idle pill records under. Whether it shows
/// and where come from the settings record, which the pill already follows.
struct HudPillState: Decodable {
    let modeName: String?
}

/// `recording-error`: the failure lane. `errorType` names it; nothing else in
/// the frame is shown.
struct RecordingErrorEvent: Decodable {
    let errorType: String
}

/// `paste-error`: the words were kept but never left Sona. `historyId` is
/// the entry that holds them, or nil when history is off.
struct PasteErrorEvent: Decodable {
    let historyId: Int64?
}

/// `rewrite-skipped`: the mode asked for a rewrite and the words went out
/// as spoken. `historyId` is the entry that keeps the receipt, or nil when
/// history is off.
struct RewriteSkippedEvent: Decodable {
    let historyId: Int64?
    let outcome: RewriteOutcome
}

/// A sentence about the last dictation, on the capture page until dismissed
/// or the next recording: the cause, the way out, and when the words were
/// kept, the entry to open.
struct CaptureNotice: Equatable {
    let text: String
    var dictation: Int64? = nil
}

/// One line of the idle pill's menu.
struct PillMode: Identifiable, Equatable {
    let id: String
    let name: String
    let active: Bool
}

extension CoreEvent {
    static let recordingError = "recording-error"
    static let pasteError = "paste-error"
    static let rewriteSkipped = "rewrite-skipped"
    static let transcriptionError = "transcription-error"
    /// Sixteen frequency buckets, each 0 to 1, about twenty-four times a
    /// second while the microphone is open and the overlay style shows them.
    static let micLevel = "mic-level"
}

/// Everything the windows share. One instance, on the main actor, handed to
/// every scene through the environment. The core is spawned here, every slice
/// store is created here against it, and what the slices leave to "the
/// integrator" — navigation between them, the floating panels, the login
/// item — lives here.
@MainActor
@Observable
final class AppModel {
    var place: Place = .capture
    var settingsPlace: SettingsPlace = .essentials
    var showingSettings = false
    var paletteShown = false
    /// The question a `sona://search` link carried, held until the palette
    /// opens and reads it.
    private var paletteSeed = ""
    var sheet: Sheet?

    /// The core answered its first request. Nothing that reads the core is
    /// drawn before this.
    private(set) var ready = false
    /// The core's socket closed under the shell: the process died or hung up.
    /// Every request in flight failed, and nothing works until a restart.
    private(set) var coreStopped = false
    /// A restart of the stopped core is in flight.
    private(set) var coreRestarting = false
    /// The microphone follows this: on while recording, off otherwise. The
    /// core decides; the shell only asks and follows the events.
    private(set) var capture: CaptureState = .idle
    /// The words of the dictation in flight: committed text, then what the
    /// engine still expects to revise.
    private(set) var liveText = ""
    /// The last thing the core could not do, shown until the next success.
    private(set) var coreError: String?
    /// The last failure the core announced about a dictation, until dismissed
    /// or the next recording starts.
    private(set) var notice: CaptureNotice?
    private(set) var models: [Model] = []
    private(set) var currentModelId = ""
    /// The catalog has been read once. Before that, an empty list is not
    /// "no models".
    private(set) var modelsLoaded = false
    /// Why the catalog could not be read, until a read works.
    private(set) var modelsError: String?
    /// The phase each model is in beyond the catalog's own record, by id.
    /// The core's events move a phase along; a command's refusal ends it.
    private(set) var modelOperations: [String: ModelOperation] = [:]
    /// The mode the idle pill records under, as the core names it.
    private(set) var pillMode: String?
    /// Every mode, for the pill's right-click menu.
    private(set) var pillModes: [PillMode] = []

    let meter = LevelMeter()

    let settings: SettingsStore
    let onboarding: OnboardingStore
    let secureInput: SecureInputStore
    let overview: OverviewStore
    let library: LibraryStore
    let meetings: MeetingsStore
    let live: MeetingLiveStore
    let meetingSettings: MeetingSettingsStore
    let people: PeopleStore
    let modes: ModesStore
    let providers: ProvidersStore
    let vocabulary: VocabularyStore
    let prompts: PromptsStore
    let workflows: WorkflowsStore
    let agents: AgentBridgeStore
    let pairing: AgentPairingStore
    let chat: ChatStore
    let cloudSync: CloudSyncStore
    let privacy: PrivacyStore
    let imports: ImportStore
    let documents: DocumentStore
    let query: QueryStore
    let recorder: RecorderStore
    let about: AboutStore
    let debug: DebugStore

    @ObservationIgnored let core = Core()
    @ObservationIgnored private let pill = FloatingPanel()
    @ObservationIgnored private let consent = FloatingPanel()
    /// Presents the main window, as the scene's `openWindow` does. Only a
    /// view reaches that action, so the shell hands it over when it first
    /// appears, which is at launch: the scene presents the window then.
    @ObservationIgnored var presentMainWindow: () -> Void = {}

    init() {
        settings = SettingsStore(core: core)
        onboarding = OnboardingStore(core: core)
        secureInput = SecureInputStore(core: core)
        overview = OverviewStore(core: core)
        library = LibraryStore(core: core)
        meetings = MeetingsStore(core: core)
        live = MeetingLiveStore(core: core)
        meetingSettings = MeetingSettingsStore(core: core)
        people = PeopleStore(core: core)
        modes = ModesStore(core: core)
        providers = ProvidersStore(core: core)
        vocabulary = VocabularyStore(core: core)
        prompts = PromptsStore(core: core)
        workflows = WorkflowsStore(core: core)
        agents = AgentBridgeStore(core: core)
        pairing = AgentPairingStore(core: core)
        chat = ChatStore(core: core)
        cloudSync = CloudSyncStore(core: core)
        privacy = PrivacyStore(core: core)
        imports = ImportStore(core: core)
        documents = DocumentStore(core: core)
        query = QueryStore(core: core)
        recorder = RecorderStore(core: core)
        about = AboutStore(core: core)
        debug = DebugStore(core: core)

        for name in [
            CoreEvent.activity, CoreEvent.streamText, CoreEvent.streamPhase, CoreEvent.micLevel,
            CoreEvent.modelStateChanged, CoreEvent.modelsUpdated, CoreEvent.downloadProgress,
            CoreEvent.downloadComplete, CoreEvent.downloadFailed, CoreEvent.downloadCancelled,
            CoreEvent.verificationStarted, CoreEvent.verificationCompleted,
            CoreEvent.extractionStarted, CoreEvent.extractionCompleted, CoreEvent.extractionFailed,
            CoreEvent.modelDeleted, CoreEvent.recordingError, CoreEvent.pasteError, CoreEvent.rewriteSkipped,
            CoreEvent.transcriptionError, CoreEvent.settingsChanged, CoreEvent.modesChanged,
            CoreEvent.meetingNavigationRequested,
        ] {
            core.observe(name) { [weak self] line in self?.handle(name, line) }
        }
        query.onLink = { [weak self] target in self?.open(target) }
        imports.jobCompleted = { [weak self] _ in
            Task { await self?.library.start() }
        }
        core.onClose { [weak self] in self?.coreClosed() }
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: nil
        ) { [core] _ in
            core.shutdown()
        }
        Task { await start() }
    }

    var activeModel: Model? { models.first { $0.status == .active } }

    /// The agent may hear a question typed into the palette.
    var canAsk: Bool { chat.packs && !chat.composerDisabled }

    /// The places and verbs the palette offers before the plane is asked.
    var paletteActions: [PaletteAction] {
        var actions = Place.allCases.map { place in
            PaletteAction(id: "go-\(place.rawValue)", group: .navigation, title: place.title) { [weak self] in
                self?.go(place)
            }
        }
        actions.append(PaletteAction(id: "go-settings", group: .navigation, title: "Settings") { [weak self] in
            self?.showSettings(.essentials)
        })
        if let title = captureActionTitle {
            actions.append(PaletteAction(id: "toggle-recording", group: .actions, title: title) { [weak self] in
                self?.toggleCapture()
            })
        }
        actions.append(PaletteAction(id: "record-meeting", group: .actions, title: "Record a meeting") { [weak self] in
            self?.recordMeeting()
        })
        actions.append(PaletteAction(id: "record-screen", group: .actions, title: "Record the screen") { [weak self] in
            self?.recordScreen()
        })
        actions.append(PaletteAction(id: "open-chat", group: .actions, title: "Ask Sona") { [weak self] in
            self?.sheet = .chat
        })
        actions.append(PaletteAction(id: "import-audio", group: .actions, title: "Import audio") { [weak self] in
            self?.showSettings(.importing)
        })
        return actions
    }

    // MARK: Navigation

    func go(_ place: Place) {
        self.place = place
        showingSettings = false
    }

    func showSettings(_ tab: SettingsPlace) {
        settingsPlace = tab
        showingSettings = true
    }

    /// The main window, in front and key. A floating card, the menu bar, a
    /// link, or the core sends a person somewhere in the window: this comes
    /// first, so the place is not set on a window that is closed or behind.
    func reveal() {
        presentMainWindow()
        NSApp.activate()
    }

    /// The settings, on the tab last shown, from the app menu or the menu bar.
    func openSettings() {
        reveal()
        showSettings(settingsPlace)
    }

    /// ⌘K: the palette, over the main window.
    func toggleSearch() {
        reveal()
        paletteShown.toggle()
    }

    /// A meeting recorded by hand, from wherever a person asks: the meetings
    /// page comes forward and the recording starts.
    func recordMeeting() {
        reveal()
        go(.meetings)
        live.startManual()
    }

    func recordScreen() {
        reveal()
        sheet = .recorder
    }

    /// A retained meeting, by id, on its own page.
    func openMeeting(_ sessionId: MeetingSessionId) {
        meetings.open(sessionId)
        go(.meetings)
    }

    /// One kept dictation, open in the library: the words the notice is about.
    func openDictation(_ id: Int64) {
        library.reveal(id)
        go(.library)
    }

    func openPerson(_ id: String) {
        people.openPerson(id)
        go(.people)
    }

    /// A `sona://` link: the core resolves it and answers with a navigation
    /// event, which lands in `open(_ target:)` or the meetings store.
    func open(link: String) {
        Task { await query.open(link: link) }
    }

    /// The agent hears the palette's question in the chat sheet.
    func ask(_ question: String) {
        chat.draft = question
        sheet = .chat
        chat.send()
    }

    /// A `sona://` noun the core resolved: the target names the exact row,
    /// and the shell lands on it, not merely on its page.
    private func open(_ target: QueryLinkTarget) {
        reveal()
        switch target {
        case let .person(id):
            openPerson(id)
        case let .organization(slug):
            people.openOrganization(slug)
            go(.people)
        case let .dictation(historyId):
            openDictation(historyId)
        case let .search(question):
            paletteSeed = question
            paletteShown = true
        }
        query.clearLinkRequest()
    }

    /// What the palette's field opens holding, read once: a link's question,
    /// or nothing for a ⌘K.
    func takePaletteSeed() -> String {
        defer { paletteSeed = "" }
        return paletteSeed
    }

    // MARK: Capture

    /// What the start/stop action reads right now, or nil while the words
    /// are being worked on and there is nothing to press.
    var captureActionTitle: String? {
        switch capture {
        case .idle: "Start recording"
        case .recording: "Stop recording"
        case .working: nil
        }
    }

    /// The "Start recording" / "Stop recording" action of the page, the
    /// palette, the menu bar, the pill, and ⌘R.
    func toggleCapture() {
        switch capture {
        case .idle: startCapture()
        case .recording: stopCapture()
        case .working: break
        }
    }

    /// The same intent channel as the shortcut, the tray, and the pill.
    func startCapture() {
        call { try await self.core.request("hud_toggle_recording") }
    }

    /// A stop, not a toggle: it ends whatever is recording, whichever
    /// shortcut opened it, and is never remembered as a deferred start. The
    /// toggle the shortcut uses would remember a press made while the words
    /// are being worked on and open the microphone again when they land.
    func stopCapture() {
        call { try await self.core.request("finish_recording") }
    }

    func cancelCapture() {
        call { try await self.core.request("cancel_operation") }
    }

    func dismissNotice() {
        notice = nil
    }

    private func setCapture(_ state: CaptureState) {
        capture = state
        if case .recording = state {
            meter.start()
        } else {
            meter.stop()
        }
    }

    // MARK: Models

    func use(_ model: Model) {
        operate(model.id, .loading, "set_active_model")
    }

    func download(_ model: Model) {
        operate(model.id, .starting, "download_model")
    }

    func cancelDownload(_ model: Model) {
        modelOperations[model.id] = nil
        call { try await self.core.request("cancel_download", ["modelId": model.id]) }
    }

    func remove(_ model: Model) {
        modelOperations[model.id] = nil
        call { try await self.core.request("delete_model", ["modelId": model.id]) }
    }

    func rescanModels() {
        call {
            try await self.core.request("rescan_local_models")
            try await self.loadModels()
        }
    }

    /// Reads the catalog again after a failed read.
    func reloadModels() {
        Task { await loadModels() }
    }

    /// Takes a failure off its row.
    func dismissModelFailure(_ id: String) {
        if modelOperations[id]?.failure != nil { modelOperations[id] = nil }
    }

    /// A model command: its phase goes up at dispatch, and its refusal stays
    /// on that row, not under the page title. The answer ends whatever phase
    /// the events did not, because the command returns only when it is done.
    private func operate(_ id: String, _ phase: ModelOperation, _ method: String) {
        modelOperations[id] = phase
        Task {
            do {
                try await core.request(method, ["modelId": id])
                if modelOperations[id]?.failure == nil { modelOperations[id] = nil }
            } catch {
                modelOperations[id] = .failed(error.localizedDescription)
            }
        }
    }

    // MARK: Core

    /// Spawns the core, then starts every store that has no screen of its own
    /// to start it, each on its own: a keychain prompt holding one read must
    /// not hide the rest. Typing into other apps and the global shortcuts
    /// start from onboarding, once Accessibility is known to be allowed.
    /// The tracks begin once, at the first start that works; a restart only
    /// reloads the stores.
    private func start() async {
        do {
            try await core.start()
        } catch {
            coreError = error.localizedDescription
            return
        }
        coreStopped = false
        load()
        if ready { return }
        ready = true
        Task { await showWhatsNew() }
        track { [weak self] in self?.syncPill() }
        track { [weak self] in self?.syncConsent() }
        track { [weak self] in self?.syncNavigation() }
    }

    /// Every store reads the core: at the first start, and again after a
    /// restart, since the new core knows nothing of what the stores showed.
    private func load() {
        Task { await onboarding.start() }
        Task { await settings.start() }
        Task { await secureInput.start() }
        Task { await overview.start() }
        Task { await live.start() }
        Task { await meetings.start() }
        Task { await meetingSettings.start() }
        Task { await people.start() }
        Task { await vocabulary.start() }
        Task { await prompts.start() }
        Task { await workflows.start() }
        Task { await agents.start() }
        Task { await cloudSync.start() }
        Task { await privacy.start() }
        Task { await imports.start() }
        Task { await documents.start() }
        Task { await query.start() }
        Task { await about.start() }
        Task { await debug.start() }
        Task { await self.loadModels() }
        call { try await self.loadPill() }
    }

    /// The socket closed under the shell. A recording in flight is over with
    /// the process that held the microphone; the screen keeps what it had
    /// and says the core needs a restart.
    private func coreClosed() {
        coreStopped = true
        liveText = ""
        setCapture(.idle)
    }

    /// Starts the core again: after it stopped under the shell, or after the
    /// first start failed. A failure stays on screen with the way to try again.
    func restartCore() {
        guard coreStopped || !ready, !coreRestarting else { return }
        coreRestarting = true
        coreError = nil
        Task {
            await start()
            coreRestarting = false
        }
    }

    private func showWhatsNew() async {
        await debug.whatsNew.start()
        if debug.whatsNew.shouldShowOnLaunch, sheet == nil {
            sheet = .whatsNew
        }
    }

    /// Runs one request and keeps its failure on screen until the next success.
    private func call(_ work: @escaping @MainActor () async throws -> Void) {
        Task {
            do {
                try await work()
                coreError = nil
            } catch {
                coreError = error.localizedDescription
            }
        }
    }

    /// Re-runs `apply` every time something it read changes.
    private func track(_ apply: @escaping @MainActor () -> Void) {
        withObservationTracking(apply) {
            Task { @MainActor [weak self] in self?.track(apply) }
        }
    }

    /// The catalog and which model is current. A failed read keeps the last
    /// list and says why, on the page that reads it.
    private func loadModels() async {
        do {
            currentModelId = try await core.request("get_current_model")
            let infos: [ModelInfo] = try await core.request("get_available_models")
            models = infos.map { Model($0, current: currentModelId) }
            modelsError = nil
        } catch {
            modelsError = "Couldn't read the models. \(error.localizedDescription)"
        }
        modelsLoaded = true
    }

    /// What the idle pill shows and what its menu offers: the active mode's
    /// name, and every mode by name for the right-click menu. `get_modes` is
    /// a settings read, so the menu costs no keychain prompt at launch.
    private func loadPill() async throws {
        let state: HudPillState = try await core.request("hud_pill_state")
        pillMode = state.modeName
        let snapshot: ModesSnapshot = try await core.request("get_modes")
        pillModes = snapshot.modes.map { PillMode(id: $0.id, name: $0.name, active: $0.id == snapshot.activeModeId) }
    }

    /// The pill's menu chose a mode. The core answers with `modes-changed`,
    /// which reads the pill again; a refusal lands in `coreError`.
    func choosePillMode(_ id: String) {
        call { try await self.core.request("set_active_mode", ["modeId": id]) }
    }

    // MARK: Floating panels

    /// While recording, the sound at the overlay's edge unless the overlay is
    /// off; idle, the mode pill at its own edge when it is on. One panel,
    /// moved between the two. No pill while the core is stopped: it would
    /// offer a recording nothing can make.
    private func syncPill() {
        let record = settings.settings
        if coreStopped {
            pill.hide()
        } else if capture != .idle {
            if record.overlayStyle == .none {
                pill.hide()
            } else {
                pill.show(HUDPill().environment(self), at: .edge(record.overlayPosition))
            }
        } else if record.hudPillEnabled {
            pill.show(HUDPill().environment(self), at: .edge(record.hudPillPosition))
        } else {
            pill.hide()
        }
    }

    /// The consent panel: an offer to record, the recording in progress, a
    /// prep or wrap card. The view draws its own surface; this only floats
    /// it at the top right of the screen the pointer is on, as the Tauri
    /// window did.
    private func syncConsent() {
        guard live.card != nil else {
            consent.hide()
            return
        }
        let view = MeetingConsentPanelView(
            store: live,
            onOpenBrief: { [weak self] id in
                self?.reveal()
                self?.openMeeting(id)
            },
            onOpenNotes: { [weak self] id in
                self?.reveal()
                self?.openMeeting(id)
            },
            followUp: { [core] id in
                let draft: MeetingFollowUpDraft = try await core.request(
                    "meeting_follow_up_draft", MeetingRequest.followUpDraft(id))
                return draft.body
            }
        )
        .padding(8)
        .environment(self)
        consent.show(view, at: .topTrailing)
    }

    /// The cues the meeting store leaves for the shell: a stopped or imported
    /// meeting to read, the digest asking for Capture.
    private func syncNavigation() {
        if let opened = live.opened {
            reveal()
            openMeeting(opened)
            live.clearOpened()
        }
        if live.captureRequested {
            reveal()
            go(.capture)
            live.clearCaptureRequest()
        }
    }

    // MARK: Events

    private func handle(_ name: String, _ line: Data) {
        do {
            switch name {
            case CoreEvent.activity:
                let activity: DictationActivity = try Core.payload(line)
                switch activity.state {
                case "recording":
                    liveText = ""
                    notice = nil
                    setCapture(.recording(since: .now))
                case "transcribing":
                    setCapture(.working("transcribing"))
                default:
                    setCapture(.idle)
                }
            case CoreEvent.streamText:
                let text: StreamText = try Core.payload(line)
                liveText = text.tentative.isEmpty ? text.committed : text.committed + " " + text.tentative
            case CoreEvent.micLevel:
                let buckets: [Float] = try Core.payload(line)
                meter.push(buckets)
            case CoreEvent.streamPhase:
                let phase: StreamPhase = try Core.payload(line)
                if phase.phase == "working", let kind = phase.kind {
                    setCapture(.working(kind))
                }
            case CoreEvent.downloadProgress:
                let progress: DownloadProgress = try Core.payload(line)
                if let index = models.firstIndex(where: { $0.id == progress.modelId }) {
                    models[index].status = .downloading(
                        fraction: progress.total == 0 ? 0 : Double(progress.downloaded) / Double(progress.total),
                        downloaded: "\(Model.bytes(progress.downloaded)) of \(Model.bytes(progress.total))")
                }
                // Bytes are arriving: the server answered.
                if modelOperations[progress.modelId] == .starting { modelOperations[progress.modelId] = nil }
            case CoreEvent.verificationStarted:
                let id: String = try Core.payload(line)
                modelOperations[id] = .verifying
            case CoreEvent.extractionStarted:
                let id: String = try Core.payload(line)
                modelOperations[id] = .extracting
            case CoreEvent.verificationCompleted, CoreEvent.extractionCompleted:
                let id: String = try Core.payload(line)
                endModelPhase(id)
            case CoreEvent.downloadFailed, CoreEvent.extractionFailed:
                let failure: ModelFailure = try Core.payload(line)
                modelOperations[failure.modelId] = .failed(failure.error)
                Task { await loadModels() }
            case CoreEvent.downloadComplete, CoreEvent.downloadCancelled, CoreEvent.modelDeleted:
                let id: String = try Core.payload(line)
                endModelPhase(id)
                Task { await loadModels() }
            case CoreEvent.modelStateChanged:
                let change: ModelStateChange = try Core.payload(line)
                switch change.eventType {
                case "loading_started":
                    if let id = change.modelId { modelOperations[id] = .loading }
                case "loading_failed":
                    if let id = change.modelId {
                        modelOperations[id] = .failed(change.error ?? "The model could not be loaded.")
                    }
                case "unloaded" where change.error != nil:
                    /* The engine crashed under the model: the model is fine,
                     * the next dictation loads it again, and the person who
                     * was dictating is the one who needs to hear it. */
                    notice = CaptureNotice(text: "The speech engine stopped and unloaded the model. It loads again on the next dictation.")
                default:
                    if let id = change.modelId { endModelPhase(id) }
                }
                Task { await loadModels() }
            case CoreEvent.modelsUpdated:
                Task { await loadModels() }
            case CoreEvent.settingsChanged, CoreEvent.modesChanged:
                call { try await self.loadPill() }
            case CoreEvent.recordingError:
                let event: RecordingErrorEvent = try Core.payload(line)
                notice = CaptureNotice(text: Self.recordingErrorText(event.errorType))
            case CoreEvent.pasteError:
                let event: PasteErrorEvent = try Core.payload(line)
                notice = CaptureNotice(
                    text: event.historyId == nil
                        ? "The words could not be pasted into the app in front. Focus a text field and try again."
                        : "The words could not be pasted into the app in front. They are kept in the Library.",
                    dictation: event.historyId)
            case CoreEvent.rewriteSkipped:
                let event: RewriteSkippedEvent = try Core.payload(line)
                if let text = event.outcome.skippedText {
                    notice = CaptureNotice(text: text, dictation: event.historyId)
                }
            case CoreEvent.transcriptionError:
                notice = CaptureNotice(text: "Couldn't transcribe. Try again.")
            case CoreEvent.meetingNavigationRequested:
                reveal()
                go(.meetings)
            default:
                break
            }
        } catch {
            coreError = "\(name): \(error.localizedDescription)"
        }
    }

    /// A phase ended by the core. A failure is not a phase: it stays until
    /// dismissed or the next attempt at the same model.
    private func endModelPhase(_ id: String) {
        if modelOperations[id]?.failure == nil { modelOperations[id] = nil }
    }

    /// One sentence each: the cause, then the way out. The same sentences
    /// `App.tsx` showed as toasts; the three the web app never named get the
    /// HUD's short form.
    private static func recordingErrorText(_ errorType: String) -> String {
        switch errorType {
        case "microphone_permission_denied":
            "Grant microphone access in System Settings → Privacy & Security → Microphone."
        case "no_input_device":
            "No audio input device was detected. Connect a microphone and try again."
        case "no_speech_detected":
            "No speech was detected. A sample of the recording was saved to History."
        case "no_model_selected":
            "No speech model is selected. Choose one in Settings > Models."
        case "model_not_downloaded":
            "The selected speech model isn't downloaded. Download it in Settings > Models."
        case "command_no_selection":
            "Select the text you want to change, then hold the command shortcut and say the change."
        case "command_rewrite_unavailable":
            "The rewrite returned nothing, so your selection was left as it was. Check the provider under Settings > Models > Language models and try again."
        case "no_speech_save_failed":
            "No speech was detected, and the sample could not be saved to History. Check the disk and try again."
        case "capture_overrun":
            "Recording cut short."
        case "cloud_unavailable":
            "Cloud unavailable."
        case "cloud_transcription_held":
            "Sona held the cloud result: nothing trustworthy came back and no local model was available."
        default:
            "Couldn't start recording: Unknown error. Try again."
        }
    }
}

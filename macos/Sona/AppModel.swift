import AppKit
import Foundation
import Observation
import ServiceManagement
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

extension CoreEvent {
    static let recordingError = "recording-error"
    static let pasteError = "paste-error"
    static let transcriptionError = "transcription-error"
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
    var sheet: Sheet?

    /// The core answered its first request. Nothing that reads the core is
    /// drawn before this.
    private(set) var ready = false
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
    private(set) var notice: String?
    private(set) var models: [Model] = []
    private(set) var currentModelId = ""
    /// The mode the idle pill records under, as the core names it.
    private(set) var pillMode: String?

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
            CoreEvent.activity, CoreEvent.streamText, CoreEvent.streamPhase,
            CoreEvent.modelStateChanged, CoreEvent.modelsUpdated, CoreEvent.downloadProgress,
            CoreEvent.downloadComplete, CoreEvent.downloadFailed, CoreEvent.downloadCancelled,
            CoreEvent.modelDeleted, CoreEvent.recordingError, CoreEvent.pasteError,
            CoreEvent.transcriptionError, CoreEvent.settingsChanged, CoreEvent.modesChanged,
            CoreEvent.meetingNavigationRequested,
        ] {
            core.observe(name) { [weak self] line in self?.handle(name, line) }
        }
        settings.onAutostartChanged = { enabled in
            Task { try? await LoginItem.apply(enabled) }
        }
        query.onLink = { [weak self] target in self?.open(target) }
        imports.jobCompleted = { [weak self] _ in
            Task { await self?.library.start() }
        }
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
        actions.append(PaletteAction(
            id: "toggle-recording", group: .actions,
            title: capture == .idle ? "Start recording" : "Stop recording"
        ) { [weak self] in
            self?.toggleCapture()
        })
        actions.append(PaletteAction(id: "record-meeting", group: .actions, title: "Record a meeting") { [weak self] in
            self?.go(.meetings)
            self?.live.startManual()
        })
        actions.append(PaletteAction(id: "record-screen", group: .actions, title: "Record the screen") { [weak self] in
            self?.sheet = .recorder
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

    /// A retained meeting, by id, on its own page.
    func openMeeting(_ sessionId: MeetingSessionId) {
        meetings.open(sessionId)
        go(.meetings)
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

    private func open(_ target: QueryLinkTarget) {
        switch target {
        case let .person(id):
            openPerson(id)
        case .organization:
            go(.people)
        case .dictation:
            go(.library)
        case .search:
            paletteShown = true
        }
        query.clearLinkRequest()
    }

    // MARK: Capture

    func toggleCapture() {
        call { try await self.core.request("hud_toggle_recording") }
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
        call { try await self.core.request("set_active_model", ["modelId": model.id]) }
    }

    func download(_ model: Model) {
        call { try await self.core.request("download_model", ["modelId": model.id]) }
    }

    func cancelDownload(_ model: Model) {
        call { try await self.core.request("cancel_download", ["modelId": model.id]) }
    }

    func remove(_ model: Model) {
        call { try await self.core.request("delete_model", ["modelId": model.id]) }
    }

    func rescanModels() {
        call { try await self.core.request("rescan_local_models") }
    }

    // MARK: Core

    /// Spawns the core, then starts every store that has no screen of its own
    /// to start it, each on its own: a keychain prompt holding one read must
    /// not hide the rest. Typing into other apps and the global shortcuts
    /// start from onboarding, once Accessibility is known to be allowed.
    private func start() async {
        do {
            try await core.start()
        } catch {
            coreError = error.localizedDescription
            return
        }
        ready = true
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
        Task { await showWhatsNew() }
        call { try await self.loadModels() }
        call { try await self.loadPill() }
        track { [weak self] in self?.syncPill() }
        track { [weak self] in self?.syncConsent() }
        track { [weak self] in self?.syncNavigation() }
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

    private func loadModels() async throws {
        currentModelId = try await core.request("get_current_model")
        let infos: [ModelInfo] = try await core.request("get_available_models")
        models = infos.map { Model($0, current: currentModelId) }
    }

    private func loadPill() async throws {
        let state: HudPillState = try await core.request("hud_pill_state")
        pillMode = state.modeName
    }

    // MARK: Floating panels

    /// While recording, the sound at the overlay's edge unless the overlay is
    /// off; idle, the mode pill at its own edge when it is on. One panel,
    /// moved between the two.
    private func syncPill() {
        let record = settings.settings
        if capture != .idle {
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
    /// prep or wrap card. Floats at the top right of the screen the pointer
    /// is on, as the Tauri window did.
    private func syncConsent() {
        guard live.card != nil else {
            consent.hide()
            return
        }
        let view = MeetingConsentPanelView(
            store: live,
            onOpenBrief: { [weak self] id in self?.openMeeting(id) },
            onOpenNotes: { [weak self] id in self?.openMeeting(id) },
            followUp: { [core] id in
                let draft: MeetingFollowUpDraft? = try? await core.request(
                    "meeting_follow_up_draft", MeetingRequest.followUpDraft(id))
                return draft?.body
            }
        )
        .background(Theme.surface, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.border))
        .padding(8)
        .environment(self)
        consent.show(view, at: .topTrailing)
    }

    /// The cues the meeting store leaves for the shell: a stopped or imported
    /// meeting to read, the digest asking for Capture.
    private func syncNavigation() {
        if let opened = live.opened {
            openMeeting(opened)
            live.clearOpened()
        }
        if live.captureRequested {
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
            case CoreEvent.modelStateChanged, CoreEvent.modelsUpdated, CoreEvent.downloadComplete,
                 CoreEvent.downloadFailed, CoreEvent.downloadCancelled, CoreEvent.modelDeleted:
                call { try await self.loadModels() }
            case CoreEvent.settingsChanged, CoreEvent.modesChanged:
                call { try await self.loadPill() }
            case CoreEvent.recordingError:
                let event: RecordingErrorEvent = try Core.payload(line)
                notice = Self.recordingErrorText(event.errorType)
            case CoreEvent.pasteError:
                notice = "The transcript could not be pasted into the active app. Focus a text field and try again."
            case CoreEvent.transcriptionError:
                notice = "Couldn't transcribe. Try again."
            case CoreEvent.meetingNavigationRequested:
                go(.meetings)
            default:
                break
            }
        } catch {
            coreError = "\(name): \(error.localizedDescription)"
        }
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
            "No transcription model selected. Choose one in Settings > Models."
        case "command_no_selection":
            "Select the text you want to change, then hold the command shortcut and say the change."
        case "command_rewrite_unavailable":
            "The rewrite returned nothing, so your selection was left as it was. Check the provider in Settings > Post-processing and try again."
        case "no_speech_save_failed":
            "Couldn't start recording: Some recordings could not be imported into the vocabulary. Try again."
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

/// This bundle's own login item, kept equal to the core's `autostart_enabled`.
enum LoginItem {
    /// The status read is a round-trip to the background-task service, about
    /// two seconds on a cold launch, so it runs off the main actor.
    static func apply(_ enabled: Bool) async throws {
        try await Task.detached(priority: .utility) {
            let service = SMAppService.mainApp
            switch (enabled, service.status) {
            case (true, .enabled), (false, .notRegistered), (false, .notFound):
                return
            case (true, _):
                try service.register()
            case (false, _):
                try service.unregister()
            }
        }.value
    }
}

import AppKit
import Foundation
import Observation
import SwiftUI

/// The places a window shows. Library opens from Home and Calendar from
/// Projects, so the sidebar lists only the rest.
enum Place: Int, CaseIterable, Identifiable {
    case capture, library, meetings, people, scratchpad, projects, calendar, tasks, triage, today

    static let sidebar: [Place] = [.capture, .tasks, .triage, .meetings, .people, .scratchpad, .projects]

    var id: Int { rawValue }

    /// The sidebar row that stays lit while this place shows.
    var parent: Place {
        switch self {
        case .library: .capture
        case .calendar: .projects
        case .today: .tasks
        case .capture, .meetings, .people, .scratchpad, .projects, .tasks, .triage: self
        }
    }

    var title: String {
        switch self {
        case .capture: "Home"
        case .library: "Library"
        case .meetings: "Meetings"
        case .people: "People"
        case .scratchpad: "Notes"
        case .projects: "Projects"
        case .calendar: "Calendar"
        case .tasks: "Tasks"
        case .triage: "Triage"
        case .today: "Today"
        }
    }

    var icon: String {
        switch self {
        case .capture: "house"
        case .library: "books.vertical"
        case .meetings: "video"
        case .people: "person.2"
        case .scratchpad: "square.and.pencil"
        case .projects: "folder"
        case .calendar: "calendar"
        case .tasks: "checklist"
        case .triage: "tray"
        case .today: "sun.max"
        }
    }
}

/// Settings' pages, in the order its list shows them.
enum SettingsPlace: Int, CaseIterable, Identifiable {
    case essentials, dictation, models, modes, vocabulary, prompts, workflows, meetings
    case agents, sync, privacy, importing, documents, about, debug
    case connections

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
        case .connections: "Connections"
        }
    }
}

/// What floats over the main window. One at a time.
enum Sheet: Identifiable, Hashable {
    case chat
    case recorder
    case whatsNew
    case brief
    /// The draft that asks someone about a task they owe, by item id.
    case nudge(String)
    /// What is open with the people of a meeting, or with one person.
    case workBrief(WorkBriefSubject)

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
/// the entry that holds them, or nil when history is off. `focusMoved` says
/// the window changed while they were transcribed, so the core holds them
/// for "Paste here".
struct PasteErrorEvent: Decodable {
    let historyId: Int64?
    let focusMoved: Bool
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

/// `dictation-duration-warning-event`: the core has counted the dictation
/// to its last minute, or the count is over. `remainingSeconds` is nil then.
struct DictationDurationWarningEvent: Decodable {
    let remainingSeconds: Int?
}

enum DictationDeliveryOutcome: String, Decodable {
    case pasted, sent
}

struct DictationDeliveryEvent: Decodable {
    let outcome: DictationDeliveryOutcome
}

/// `dictation-kept`: a cancel kept the recording as a Library entry instead
/// of throwing it away.
struct DictationKeptEvent: Decodable {
    let historyId: Int64
    let durationMs: UInt64
}

enum PillFeedback: Equatable {
    case delivered(DictationDeliveryOutcome)
    case failed
    case kept
    /// The words wait for a click to paste them into the window in front.
    case held

    var label: String {
        switch self {
        case .delivered(.pasted): "Pasted"
        case .delivered(.sent): "Sent"
        case .failed: "Couldn't paste"
        case .kept: "Recording kept"
        case .held: "Paste here"
        }
    }

    var symbol: String {
        switch self {
        case .delivered(.pasted): "checkmark"
        case .delivered(.sent): "arrow.up.right"
        case .failed: "exclamationmark"
        case .kept: "tray.and.arrow.down"
        case .held: "doc.on.clipboard"
        }
    }

    /// How long the pill holds it. A line that needs reading stays longer,
    /// and an offer stays long enough to click into a field first.
    var holdMilliseconds: Int {
        switch self {
        case .delivered: 900
        case .failed, .kept: 2200
        case .held: 10_000
        }
    }
}

struct PillPresentation: Equatable {
    let capture: CaptureState
    let feedback: PillFeedback?
}

extension CoreEvent {
    static let recordingError = "recording-error"
    static let pasteError = "paste-error"
    static let rewriteSkipped = "rewrite-skipped"
    static let transcriptionError = "transcription-error"
    /// Sixteen frequency buckets, each 0 to 1, about twenty-four times a
    /// second while the microphone is open and the overlay style shows them.
    static let micLevel = "mic-level"
    static let dictationDurationWarning = "dictation-duration-warning-event"
    /// A voice command question was answered into the chat; the sheet is
    /// what shows it.
    static let commandAnswered = "command-mode://answered"
    static let showMainWindow = "show-main-window"
    static let dictationDelivery = "dictation-delivery"
    static let dictationKept = "dictation-kept"
    /// Sent once per dictation when it reaches three minutes.
    static let dictationMeetingSuggestion = "dictation-meeting-suggestion"
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
    var paletteShown = false {
        didSet { if !paletteShown { workFocus.picker = nil } }
    }
    /// The question a `sona://search` link carried, held until the palette
    /// opens and reads it.
    private var paletteSeed = ""
    @ObservationIgnored private var pendingOpenURLs: [URL] = []
    var sheet: Sheet?
    @ObservationIgnored private var sheetBeforeQuit: Sheet?

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
    /// Seconds left before the dictation reaches its limit, once the core
    /// has said so; nil the rest of the time. The pill shows it, on its own
    /// if it has to.
    private(set) var pillWarningSeconds: Int?
    private(set) var pillFeedback: PillFeedback?
    /// The dictation has run long enough that meeting mode would suit it,
    /// until the recording ends.
    private(set) var meetingSuggested = false
    /// The pill's meeting button was pressed: the meeting start opens once the
    /// dictation has been delivered, so the words still reach the app it was for.
    private var meetingAfterCapture = false
    // The panel fades after capture ends; its content changes only on the next show.
    private(set) var pillPresentation = PillPresentation(capture: .idle, feedback: nil)
    /// Bumped when the pill's hour away is over, so the pill is placed again.
    private var pillReturned = 0

    let meter = LevelMeter()

    let settings: SettingsStore
    let onboarding: OnboardingStore
    let secureInput: SecureInputStore
    let overview: OverviewStore
    let evening: EveningStore
    let library: LibraryStore
    let meetings: MeetingsStore
    let live: MeetingLiveStore
    let meetingSettings: MeetingSettingsStore
    let prep: MeetingPrepStore
    let personPhotos: PersonPhotoStore
    let people: PeopleStore
    let scratchpad: ScratchpadStore
    let projects: ProjectsStore
    let modes: ModesStore
    let providers: ProvidersStore
    let vocabulary: VocabularyStore
    let prompts: PromptsStore
    let workflows: WorkflowsStore
    let agents: AgentBridgeStore
    let pairing: AgentPairingStore
    let chat: ChatStore
    let cloudSync: CloudSyncStore
    let connections: ConnectionsStore
    let privacy: PrivacyStore
    let imports: ImportStore
    let documents: DocumentStore
    let query: QueryStore
    let recorder: RecorderStore
    let about: AboutStore
    let debug: DebugStore
    let spotlight: SpotlightStore
    let work: WorkStore
    let workFocus = WorkFocus()
    let rituals: RitualStore
    let heard: LiveCardStore

    @ObservationIgnored let core = Core()
    @ObservationIgnored let workKeys = WorkKeys()
    @ObservationIgnored private lazy var pill = FloatingPanel(HUDPill().environment(self))
    @ObservationIgnored private lazy var consent = FloatingPanel(consentPanel(), arrival: .slide)
    @ObservationIgnored private let windowAttached: WindowAttachedCard
    @ObservationIgnored private lazy var quickEntry = QuickEntry(model: self)
    @ObservationIgnored private lazy var quietReviewScheduler = QuietReviewScheduler(store: projects) { [weak self] in
        guard let self, ready, !coreStopped, !coreRestarting, onboarding.step == .done,
              capture == .idle, !live.starting, live.pending == nil, live.live == nil, live.active == nil,
              !recorder.hasCapture, !recorder.selectionPending, sheet != .recorder,
              meetingSettings.settingsRead, meetingSettings.settings.localEngine == .appleIntelligence,
              case .appleIntelligence(blocker: nil)? = meetingSettings.engineStatus else { return false }
        return true
    }
    /// What the pill showed when it was last placed.
    @ObservationIgnored private var pillCapture = CaptureState.idle
    /// The wait for the end of the pill's hour away, and the time it ends.
    @ObservationIgnored private var pillReturn: Task<Void, Never>?
    @ObservationIgnored private var pillReturnAt: Int64?
    /// The ended hide this shell last asked the core to clear.
    @ObservationIgnored private var pillClearedAt: Int64?
    @ObservationIgnored private var feedbackDismissal: Task<Void, Never>?
    /// Presents the main window, as the scene's `openWindow` does. Only a
    /// view reaches that action, and the window stays closed at launch, so
    /// the menu bar mark hands it over when it first appears. Observed, so a
    /// reveal the core asked for before then runs again once it can.
    var presentMainWindow: () -> Void = {}

    init() {
        settings = SettingsStore(core: core)
        onboarding = OnboardingStore(core: core)
        secureInput = SecureInputStore(core: core)
        overview = OverviewStore(core: core)
        evening = EveningStore(core: core)
        library = LibraryStore(core: core)
        meetings = MeetingsStore(core: core)
        live = MeetingLiveStore(core: core)
        meetingSettings = MeetingSettingsStore(core: core)
        prep = MeetingPrepStore(core: core)
        let photos = PersonPhotoStore(core: core)
        personPhotos = photos
        people = PeopleStore(core: core, photos: photos)
        scratchpad = ScratchpadStore(core: core)
        projects = ProjectsStore(core: core)
        modes = ModesStore(core: core)
        providers = ProvidersStore(core: core)
        vocabulary = VocabularyStore(core: core)
        prompts = PromptsStore(core: core)
        workflows = WorkflowsStore(core: core)
        agents = AgentBridgeStore(core: core)
        pairing = AgentPairingStore(core: core)
        chat = ChatStore(core: core)
        cloudSync = CloudSyncStore(core: core)
        connections = ConnectionsStore(core: core)
        privacy = PrivacyStore(core: core)
        imports = ImportStore(core: core)
        documents = DocumentStore(core: core)
        query = QueryStore(core: core)
        recorder = RecorderStore(core: core)
        about = AboutStore(core: core)
        debug = DebugStore(core: core)
        windowAttached = WindowAttachedCard(core: core)
        spotlight = SpotlightStore(core: core)
        work = WorkStore(core: core)
        rituals = RitualStore(core: core)
        heard = LiveCardStore(core: core)

        for name in [
            CoreEvent.activity, CoreEvent.streamText, CoreEvent.streamPhase, CoreEvent.micLevel,
            CoreEvent.modelStateChanged, CoreEvent.modelsUpdated, CoreEvent.downloadProgress,
            CoreEvent.downloadComplete, CoreEvent.downloadFailed, CoreEvent.downloadCancelled,
            CoreEvent.verificationStarted, CoreEvent.verificationCompleted,
            CoreEvent.extractionStarted, CoreEvent.extractionCompleted, CoreEvent.extractionFailed,
            CoreEvent.modelDeleted, CoreEvent.recordingError, CoreEvent.pasteError, CoreEvent.rewriteSkipped,
            CoreEvent.transcriptionError, CoreEvent.settingsChanged, CoreEvent.modesChanged,
            CoreEvent.meetingNavigationRequested, CoreEvent.dictationDurationWarning,
            CoreEvent.commandAnswered, CoreEvent.showMainWindow, CoreEvent.dictationDelivery,
            CoreEvent.dictationKept, CoreEvent.dictationMeetingSuggestion,
        ] {
            core.observe(name) { [weak self] line in self?.handle(name, line) }
        }
        observeReminders()
        library.recovered = { [weak self] ids in self?.announceRecovered(ids) }
        meetingSettings.keptLocalChanged = { [weak self] in self?.spotlight.refresh() }
        query.onLink = { [weak self] target in self?.open(target) }
        imports.jobCompleted = { [weak self] _ in
            Task { await self?.library.start() }
        }
        core.observe(QuickEntry.openEvent) { [weak self] line in self?.quickEntry.open(line) }
        core.observe(QuickEntry.filedEvent) { [weak self] line in self?.quickEntry.file(line) }
        core.onClose { [weak self] in self?.coreClosed() }
        workKeys.install(self)
        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: nil
        ) { [weak self] _ in
            Task { await self?.morningCheck() }
        }
        Task { await start() }
    }

    var activeModel: Model? { models.first { $0.status == .active } }

    /// The agent may hear a question typed into the palette.
    var canAsk: Bool { chat.packs && !chat.composerDisabled }

    /// The places and verbs the palette offers before the plane is asked.
    var paletteActions: [PaletteAction] {
        var actions = workPaletteActions + briefPaletteActions
        actions += Place.allCases.map { place in
            PaletteAction(id: "go-\(place.rawValue)", group: .navigation, title: place.title) { [weak self] in
                self?.go(place)
            }
        }
        actions.append(PaletteAction(id: "go-settings", group: .navigation, title: "Settings") { [weak self] in
            self?.showSettings(.essentials)
        })
        for page in [SettingsPlace.modes, .vocabulary, .models] {
            actions.append(PaletteAction(id: "go-settings-\(page.rawValue)", group: .navigation, title: page.title) { [weak self] in
                self?.showSettings(page)
            })
        }
        if let title = captureActionTitle {
            actions.append(PaletteAction(id: "toggle-recording", group: .actions, title: title) { [weak self] in
                self?.toggleCapture()
            })
        }
        actions.append(PaletteAction(id: "new-task", group: .actions, title: "New task",
                                     run: { [weak self] in self?.newTask() }, icon: "plus"))
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
    /// Sona is a menu bar app, so another app is in front when this runs, and
    /// macOS turns down a plain `activate()` then: the window opened behind.
    func reveal() {
        presentMainWindow()
        NSApp.activate(ignoringOtherApps: true)
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

    /// The menu bar's stop: the recording saves on its sheet, in front.
    func stopScreenRecording() {
        reveal()
        recorder.stopAndSave()
    }

    /// AppKit refuses to quit while a sheet is up, before Sona can ask about
    /// a recording, and the screen recorder is a sheet for as long as it
    /// records. Quit closes the sheet, waits for it to leave, then quits.
    func quit() {
        sheetBeforeQuit = sheet
        sheet = nil
        Task {
            for _ in 0..<20 where NSApp.windows.contains(where: { $0.attachedSheet != nil }) {
                try? await Task.sleep(for: .milliseconds(50))
            }
            // terminate: waits for Sona's answer in a nested run loop. Run
            // from this task, that loop could not start the task that
            // answers, and Sona never quit.
            RunLoop.main.perform { MainActor.assumeIsolated { NSApp.terminate(nil) } }
        }
    }

    /// The person kept recording: the sheet Quit closed comes back.
    /// AppKit drops a sheet raised before its quit loop has unwound, so the
    /// sheet waits for the main run loop.
    func restoreSheetAfterCancelledQuit() {
        let restored = sheetBeforeQuit
        sheetBeforeQuit = nil
        RunLoop.main.perform(inModes: [.default]) {
            MainActor.assumeIsolated {
                if self.sheet == nil { self.sheet = restored }
            }
        }
    }

    /// Quit waits while a capture is starting or finishing, then asks about
    /// whatever records. Cut off, a screen recording being saved is deleted
    /// and a dictation being transcribed never delivers its words.
    func waitForCapturesToSettle() async {
        while !coreStopped, captureSettling {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private var captureSettling: Bool {
        if case .working = capture { return true }
        return live.starting || recorder.phase.activity == .settling
    }

    /// Quit stopped the dictation: wait until its words are delivered.
    func waitForDictationToFinish() async {
        while !coreStopped, capture != .idle {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    func openBrief(_ eventKey: String) {
        reveal()
        prep.openBrief(eventKey)
        sheet = .brief
    }

    /// A retained meeting, by id, on its own page.
    func openMeeting(_ sessionId: MeetingSessionId) {
        meetings.open(sessionId)
        go(.meetings)
    }

    /// A past moment, open on its meeting's page. A recording on the live
    /// screen keeps going: the live notes are saved first, and the meetings
    /// page's recording row leads back.
    func openPast(_ sessionId: MeetingSessionId, segmentId: TranscriptSegmentId?) {
        Task {
            if live.live != nil {
                guard await live.flushLiveNotes() else { return }
                live.closeLive()
            }
            meetings.open(sessionId, segmentId: segmentId)
            go(.meetings)
        }
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

    var plannerPeople: [PlanningPersonChoice] {
        (people.entries ?? [])
            .map { PlanningPersonChoice(id: $0.person.id, name: $0.person.displayName) }
            .sorted {
                let order = $0.name.localizedStandardCompare($1.name)
                return order == .orderedSame ? $0.id < $1.id : order == .orderedAscending
            }
    }

    func openProject(_ id: String) {
        projects.open(id)
        go(.projects)
    }

    func openPlanningEvidence(_ evidence: PlanningEvidenceLink) {
        guard evidence.available else { return }
        switch evidence.sourceKind {
        case .meeting:
            meetings.open(
                evidence.sourceId,
                atMilliseconds: evidence.atMs.flatMap(UInt64.init(exactly:)),
                noteId: evidence.meetingNoteId
            )
            go(.meetings)
        case .note:
            open(link: "sona://note/\(evidence.sourceId)")
        case .loop:
            open(link: "sona://loop/\(evidence.sourceId)")
        case .dictation:
            if let id = Int64(evidence.sourceId) { openDictation(id) }
        case .link:
            guard let url = URL(string: evidence.sourceId) else { return }
            if url.isFileURL {
                NSWorkspace.shared.activateFileViewerSelecting([url])
            } else if let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http" {
                NSWorkspace.shared.open(url)
            }
        }
    }

    func discussProject(_ project: PlanningProject) {
        chat.openAsk(draft: "About project “\(project.title)”: ")
        sheet = .chat
    }

    func planWithSona() {
        chat.openPlanner()
        sheet = .chat
    }

    /// A `sona://` link: the core resolves it and answers with a navigation
    /// event, which lands in `open(_ target:)` or the meetings store.
    func open(link: String) {
        Task { await query.open(link: link) }
    }

    func open(url: URL) {
        guard ready && !coreStopped else {
            pendingOpenURLs.append(url)
            return
        }
        if url.scheme?.lowercased() == "sona" {
            open(link: url.absoluteString)
            return
        }
        guard url.isFileURL else { return }
        let path = url.path
        let ext = url.pathExtension.lowercased()
        if ext == "sona" {
            reveal()
            showSettings(.sync)
            cloudSync.enqueueOpenedBundle(at: path)
        } else if ImportMedia.extensions.contains(ext) {
            imports.add(paths: [path])
            reveal()
            showSettings(.importing)
        }
    }

    private func openPendingURLs() {
        let urls = pendingOpenURLs
        pendingOpenURLs.removeAll()
        for url in urls { open(url: url) }
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
        case let .note(id, versionId):
            Task { await scratchpad.open(id: id, versionId: versionId) }
            go(.scratchpad)
        case let .subject(id):
            Task { await scratchpad.openSubject(id: id) }
            go(.scratchpad)
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
        clearPillFeedback()
    }

    /// The pill's "Paste here": the words held back when the window changed
    /// go to the window in front now.
    func pasteHeldDictation() {
        clearPillFeedback()
        call { try await self.core.request("paste_held_dictation") }
    }

    /// The long dictation ends as any other, transcribed and delivered, and the
    /// meeting start opens for what comes next.
    func finishIntoMeeting() {
        meetingSuggested = false
        call {
            try await self.core.request("finish_recording")
            if case .idle = self.capture {
                self.recordMeeting()
            } else {
                self.meetingAfterCapture = true
            }
        }
    }

    func dismissNotice() {
        notice = nil
    }

    private func setCapture(_ state: CaptureState) {
        capture = state
        if case .recording = state {
            clearPillFeedback()
            meter.start()
        } else {
            meetingSuggested = false
            meter.stop()
        }
        if case .idle = state {
            dismissPillFeedbackAfterDelay()
            if meetingAfterCapture {
                meetingAfterCapture = false
                recordMeeting()
            }
        }
    }

    /// The recordings the core recovered from a dictation that never
    /// finished, in one notice that opens the newest.
    private func announceRecovered(_ ids: [Int64]) {
        let text = ids.count == 1
            ? "A dictation didn't finish last time. The recording is kept in the Library."
            : "\(ids.count) dictations didn't finish last time. The recordings are kept in the Library."
        notice = CaptureNotice(text: text, dictation: ids.max())
    }

    private func showPillFeedback(_ feedback: PillFeedback) {
        guard settings.settings.overlayStyle != .none else { return }
        // A kept recording is announced as its cancel ends the recording.
        if case .recording = capture, feedback != .kept { return }
        feedbackDismissal?.cancel()
        feedbackDismissal = nil
        pillFeedback = feedback
        if case .idle = capture {
            dismissPillFeedbackAfterDelay()
        }
    }

    private func dismissPillFeedbackAfterDelay() {
        guard let feedback = pillFeedback else { return }
        feedbackDismissal?.cancel()
        feedbackDismissal = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(feedback.holdMilliseconds))
            guard !Task.isCancelled else { return }
            self?.pillFeedback = nil
            self?.feedbackDismissal = nil
        }
    }

    private func clearPillFeedback() {
        feedbackDismissal?.cancel()
        feedbackDismissal = nil
        pillFeedback = nil
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
            await self.loadModels()
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
        if ready {
            openPendingURLs()
            return
        }
        ready = true
        Task { await showWhatsNew() }
        pill.onClick = { [weak self] in self?.toggleCapture() }
        pill.onDock = { [weak self] edge in
            guard let self else { return }
            Task { await self.settings.setHudPillPosition(edge) }
        }
        track { [weak self] in self?.syncPill() }
        track { [weak self] in self?.syncConsent() }
        track { [weak self] in self?.syncNavigation() }
        track { [weak self] in self?.quietReviewScheduler.update() }
        track { [weak self] in self?.holdPlaybackWhileCapturing() }
        openPendingURLs()
    }

    /// Every store reads the core: at the first start, and again after a
    /// restart, since the new core knows nothing of what the stores showed.
    private func load() {
        Task {
            await onboarding.start()
            self.openMorningPlanIfDue()
        }
        Task { await settings.start() }
        Task { await secureInput.start() }
        Task { await overview.start() }
        Task { await evening.start() }
        Task { await live.start() }
        Task { await meetings.start() }
        Task { await meetingSettings.start() }
        Task { await projects.start() }
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
        Task { await spotlight.start() }
        Task {
            await work.start()
            self.openMorningPlanIfDue()
        }
        Task {
            await rituals.start()
            self.openMorningPlanIfDue()
        }
        Task { await self.loadModels() }
        Task { await library.coreStarted() }
        call { try await self.loadPill() }
    }

    /// The socket closed under the shell. A recording in flight is over with
    /// the process that held the microphone; the screen keeps what it had
    /// and says the core needs a restart.
    private func coreClosed() {
        coreStopped = true
        liveText = ""
        clearPillFeedback()
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

    func pauseQuietReview() {
        quietReviewScheduler.setPaused(true)
    }

    func resumeQuietReview() {
        quietReviewScheduler.setPaused(false)
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
        withObservationTracking(apply) { [weak self] in
            Task { @MainActor [weak self] in self?.track(apply) }
        }
    }

    /// Sound played while a meeting captures lands in its call track and its
    /// microphone, so both players hold until the capture ends.
    private func holdPlaybackWhileCapturing() {
        meetings.audio.hold(live.capturing)
        library.hold(live.capturing)
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

    /// The pill's menu asked for an hour without it. The record comes back
    /// with the time it returns, which places the pill and arms the wait.
    func hidePillForAnHour() {
        Task { await settings.hideHudPillForAnHour() }
    }

    // MARK: Floating panels

    private func syncPill() {
        let record = settings.settings
        if record.overlayStyle == .none, pillFeedback != nil {
            clearPillFeedback()
        }
        let anew: Bool
        switch (pillCapture, capture) {
        case (.recording, .recording): anew = false
        case (_, .recording): anew = true
        default: anew = false
        }
        pillCapture = capture
        _ = pillReturned
        pill.dockable = false
        if coreStopped {
            pill.hide()
        } else if capture != .idle || pillFeedback != nil {
            if record.overlayStyle == .none, pillWarningSeconds == nil {
                pill.hide()
            } else {
                pillPresentation = PillPresentation(capture: capture, feedback: pillFeedback)
                pill.show(at: .edge(HudPillEdge(record.overlayPosition)), anew: anew)
            }
        } else if record.hudPillEnabled, !pillAway(record) {
            pillPresentation = PillPresentation(capture: .idle, feedback: nil)
            pill.dockable = true
            pill.show(at: .edge(record.hudPillPosition), anew: anew)
        } else {
            pill.hide()
        }
    }

    /// Whether the pill's hour away is still running. A running one arms the
    /// wait for its end. One that has ended is cleared from the record: the
    /// core does that itself when it set the hide this run, but a launch
    /// mid-hour or after it leaves only this shell to notice.
    private func pillAway(_ record: AppSettings) -> Bool {
        guard let until = record.hudPillHiddenUntilMs else {
            endPillReturn()
            return false
        }
        guard Self.now() < until else {
            endPillReturn()
            // Once per stale time: a refused clear must not be asked again
            // on the re-read that follows every write.
            if pillClearedAt != until {
                pillClearedAt = until
                Task { await settings.showHudPillNow() }
            }
            return false
        }
        if pillReturnAt != until {
            pillReturn?.cancel()
            pillReturnAt = until
            pillReturn = Task { [weak self] in
                try? await Task.sleep(for: .milliseconds(max(0, until - Self.now())))
                guard !Task.isCancelled, let self else { return }
                pillReturned += 1
            }
        }
        return true
    }

    private func endPillReturn() {
        pillReturn?.cancel()
        pillReturn = nil
        pillReturnAt = nil
    }

    /// The wall clock in the record's unit, milliseconds since 1970.
    private static func now() -> Int64 {
        Int64(Date.now.timeIntervalSince1970 * 1000)
    }

    /// The consent panel: an offer to record, the recording in progress, a
    /// prep or wrap card. The view reads the store and draws its own surface;
    /// this only places it on the screen the pointer is on: a card at the top
    /// right, a recording flush against the right edge.
    private func syncConsent() {
        if let card = live.card {
            consent.show(at: card.docksAtEdge ? .flushRight : .topTrailing)
        } else {
            consent.hide()
        }
    }

    private func consentPanel() -> some View {
        MeetingConsentPanelView(
            store: live,
            onOpenBrief: { [weak self] eventKey in self?.openBrief(eventKey) },
            onWrapUp: { [weak self] id in
                self?.reveal()
                self?.openMeeting(id)
                self?.meetings.openWrap()
            },
            followUp: { [core] id in
                let draft: MeetingFollowUpDraft = try await core.request(
                    "meeting_follow_up_draft", MeetingRequest.followUpDraft(id))
                return draft.body
            }
        )
        .environment(self)
    }

    /// The cues the stores leave for the shell: a stopped or imported
    /// meeting to read, the digest asking for Capture, and a first run or a
    /// lost permission, which only the window can walk through.
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
        switch onboarding.step {
        case .permissions, .languages, .model, .practice:
            reveal()
        case .probing, .done:
            break
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
            case CoreEvent.dictationDurationWarning:
                let event: DictationDurationWarningEvent = try Core.payload(line)
                pillWarningSeconds = event.remainingSeconds
            case CoreEvent.recordingError:
                let event: RecordingErrorEvent = try Core.payload(line)
                notice = CaptureNotice(text: Self.recordingErrorText(event.errorType))
            case CoreEvent.pasteError:
                let event: PasteErrorEvent = try Core.payload(line)
                let offered = event.focusMoved && settings.settings.overlayStyle != .none
                notice = CaptureNotice(
                    text: Self.pasteErrorText(event, offered: offered),
                    dictation: event.historyId)
                showPillFeedback(event.focusMoved ? .held : .failed)
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
            case CoreEvent.commandAnswered:
                reveal()
                sheet = .chat
            case CoreEvent.showMainWindow:
                reveal()
            case CoreEvent.dictationDelivery:
                let event: DictationDeliveryEvent = try Core.payload(line)
                showPillFeedback(.delivered(event.outcome))
            case CoreEvent.dictationKept:
                let event: DictationKeptEvent = try Core.payload(line)
                let length = LibraryClock.milliseconds(Int64(clamping: event.durationMs))
                notice = CaptureNotice(
                    text: "Cancelled. The \(length) recording is kept in the Library for 14 days.",
                    dictation: event.historyId)
                showPillFeedback(.kept)
            case CoreEvent.dictationMeetingSuggestion:
                if case .recording = capture { meetingSuggested = true }
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

    /// Why the words stayed in Sona, and where they are now. `offered` says
    /// the pill shows "Paste here".
    private static func pasteErrorText(_ event: PasteErrorEvent, offered: Bool) -> String {
        guard event.focusMoved else {
            return event.historyId == nil
                ? "The words could not be pasted into the app in front. Focus a text field and try again."
                : "The words could not be pasted into the app in front. They are kept in the Library."
        }
        return [
            "You switched windows before the words were ready, so they weren't pasted.",
            offered ? "Click Paste here on the pill to paste them where you are now." : nil,
            event.historyId == nil ? nil : "They are kept in the Library.",
        ].compactMap { $0 }.joined(separator: " ")
    }
}

import AppKit
import Foundation
import Observation

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

/// The tabs across the top of Settings.
enum SettingsPlace: Int, CaseIterable, Identifiable {
    case essentials, models, modes, vocabulary, prompts, workflows, agents, advanced

    var id: Int { rawValue }

    var title: String {
        switch self {
        case .essentials: "Essentials"
        case .models: "Models"
        case .modes: "Modes"
        case .vocabulary: "Vocabulary"
        case .prompts: "Prompts"
        case .workflows: "Workflows"
        case .agents: "Agents"
        case .advanced: "Advanced"
        }
    }
}

/// Everything the windows share. One instance, on the main actor, handed to every
/// scene through the environment. The core is spawned here and every fact the
/// screens show about dictation comes through it.
@MainActor
@Observable
final class AppModel {
    var place: Place = .capture
    var settingsPlace: SettingsPlace = .essentials
    var showingSettings = false
    /// The microphone follows this: on while recording, off otherwise. The
    /// core decides; the shell only asks and follows the events.
    private(set) var capture: CaptureState = .idle
    let meter = LevelMeter()
    @ObservationIgnored private let pill = PillPanel()

    var selectedMeeting: Meeting?
    var selectedPerson: Person?
    var selectedTranscription: Transcription?
    var selectedMode: Mode?

    var paletteShown = false
    var chatShown = false

    var historyQuery = ""
    var decisions = SampleData.decisions

    /// Dictations, newest first, as far as the pages fetched so far reach.
    private(set) var transcriptions: [Transcription] = []
    private(set) var hasMoreTranscriptions = false
    private(set) var stats: HistoryStats?
    private(set) var models: [Model] = []
    private(set) var currentModelId = ""
    /// The words of the dictation in flight: committed text, then what the
    /// engine still expects to revise.
    private(set) var liveText = ""
    /// The last thing the core could not do, shown until the next success.
    private(set) var coreError: String?

    // Settings values. The real ones the core carries arrive in `apply`;
    // the rest give the toggles something honest to show.
    var launchAtLogin = true
    var showInMenuBar = true
    var hudPill = true
    var soundOnStart = false
    var pasteAutomatically = true
    var announceMeetings = true
    var detectMeetings = true
    var cloudSync = false
    var autoUpdate = true
    var verboseLogs = false
    var pushToTalk = "⌥ Space"
    var toggleShortcut = "⌥ ⇧ Space"
    var meetingShortcut = "⌥ ⇧ M"
    var inputDevice = "System default microphone"
    var language = "English"

    @ObservationIgnored private var core: Core!
    @ObservationIgnored private var pageSize = 50

    init() {
        core = Core { [weak self] name, line in
            Task { @MainActor [weak self] in
                self?.handle(name, line)
            }
        }
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: nil
        ) { [core] _ in
            core?.shutdown()
        }
        Task { await start() }
    }

    var activeModel: Model? { models.first { $0.status == .active } }

    func toggleCapture() {
        call { try await self.core.request("hud_toggle_recording") }
    }

    func cancelCapture() {
        call { try await self.core.request("cancel_operation") }
    }

    private func setCapture(_ state: CaptureState) {
        capture = state
        if case .recording = state {
            meter.start()
            if hudPill {
                pill.show(self)
            }
        } else {
            meter.stop()
            pill.hide()
        }
    }

    func resolve(_ decision: Decision) {
        decisions.removeAll { $0.id == decision.id }
    }

    func go(_ place: Place) {
        self.place = place
        showingSettings = false
        selectedMeeting = nil
        selectedPerson = nil
        selectedTranscription = nil
        selectedMode = nil
    }

    // MARK: History

    func loadMoreTranscriptions() {
        guard hasMoreTranscriptions, let last = transcriptions.last else { return }
        call {
            let page: PaginatedHistory = try await self.core.request(
                "get_history_entries", ["cursor": last.id, "limit": Int64(self.pageSize)])
            self.transcriptions += page.entries.map(Transcription.init)
            self.hasMoreTranscriptions = page.hasMore
        }
    }

    func delete(_ transcription: Transcription) {
        call {
            try await self.core.request("delete_history_entry", ["id": transcription.id])
            if self.selectedTranscription?.id == transcription.id {
                self.selectedTranscription = nil
            }
        }
    }

    func copy(_ transcription: Transcription) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(transcription.text, forType: .string)
    }

    // MARK: Models

    func use(_ model: Model) {
        call { try await self.core.request("switch_active_model", ["model_id": model.id]) }
    }

    func download(_ model: Model) {
        call { try await self.core.request("download_model", ["model_id": model.id]) }
    }

    func cancelDownload(_ model: Model) {
        call { try await self.core.request("cancel_download", ["model_id": model.id]) }
    }

    func remove(_ model: Model) {
        call { try await self.core.request("delete_model", ["model_id": model.id]) }
    }

    func rescanModels() {
        call { try await self.core.request("rescan_local_models") }
    }

    // MARK: Core

    /// Spawns the core, then fetches each part on its own: history stays
    /// locked until the keychain prompt is answered, and that must not hide
    /// the settings or the models. Typing into other apps needs the input
    /// backend, which only starts once Accessibility is allowed, so its
    /// failure is the one an owner most needs to see.
    private func start() async {
        do {
            try await core.start()
        } catch {
            coreError = error.localizedDescription
            return
        }
        call { try await self.loadSettings() }
        call { try await self.loadModels() }
        call { try await self.loadHistory() }
        call { try await self.core.request("initialize_shortcuts") }
        call { try await self.core.request("initialize_enigo") }
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

    private func loadSettings() async throws {
        let settings: CoreSettings = try await core.request("get_app_settings")
        currentModelId = settings.selectedModel
        showInMenuBar = settings.showTrayIcon
        inputDevice = settings.selectedMicrophone ?? "System default microphone"
        language = Locale.current.localizedString(forLanguageCode: settings.selectedLanguage) ?? settings.selectedLanguage
        if let binding = settings.bindings["transcribe"] {
            pushToTalk = binding.currentBinding
            toggleShortcut = binding.currentBinding
        }
    }

    private func loadHistory() async throws {
        let page: PaginatedHistory = try await core.request("get_history_entries", ["limit": Int64(pageSize)])
        transcriptions = page.entries.map(Transcription.init)
        hasMoreTranscriptions = page.hasMore
        stats = try await core.request("get_history_stats")
    }

    private func loadModels() async throws {
        currentModelId = try await core.request("get_current_model")
        let infos: [ModelInfo] = try await core.request("get_available_models")
        models = infos.map { Model($0, current: currentModelId) }
    }

    private func handle(_ name: String, _ line: Data) {
        do {
            switch name {
            case CoreEvent.activity:
                let activity: DictationActivity = try Core.payload(line)
                switch activity.state {
                case "recording":
                    liveText = ""
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
            case CoreEvent.historyUpdate:
                let update: HistoryUpdate = try Core.payload(line)
                switch update {
                case let .added(entry):
                    transcriptions.insert(Transcription(entry), at: 0)
                case let .updated(entry):
                    if let index = transcriptions.firstIndex(where: { $0.id == entry.id }) {
                        transcriptions[index] = Transcription(entry)
                    }
                case let .deleted(id):
                    transcriptions.removeAll { $0.id == id }
                case let .toggled(id):
                    if let index = transcriptions.firstIndex(where: { $0.id == id }) {
                        transcriptions[index].saved.toggle()
                    }
                }
                call { self.stats = try await self.core.request("get_history_stats") }
            case CoreEvent.historyStorage:
                call { try await self.loadHistory() }
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
            default:
                break
            }
        } catch {
            coreError = "\(name): \(error.localizedDescription)"
        }
    }
}

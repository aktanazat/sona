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
/// scene through the environment.
@MainActor
@Observable
final class AppModel {
    var place: Place = .capture
    var settingsPlace: SettingsPlace = .essentials
    var showingSettings = false
    /// The microphone follows this: on while recording, off otherwise.
    private(set) var capture: CaptureState = .idle
    let meter = LevelMeter()

    var selectedMeeting: Meeting?
    var selectedPerson: Person?
    var selectedTranscription: Transcription?
    var selectedMode: Mode?

    var paletteShown = false
    var chatShown = false

    var historyQuery = ""
    var decisions = SampleData.decisions

    // Settings values. Real ones arrive with the bridge; these give the toggles
    // something honest to show.
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
    var inputDevice = "MacBook Pro Microphone"
    var language = "English"

    var activeModel: Model? { SampleData.models.first { $0.status == .active } }

    func toggleCapture() {
        switch capture {
        case .idle, .paused:
            setCapture(.recording(since: .now))
        case .recording:
            setCapture(.idle)
        }
    }

    func pauseCapture() {
        if case let .recording(since) = capture {
            setCapture(.paused(elapsed: Date.now.timeIntervalSince(since)))
        }
    }

    func setCapture(_ state: CaptureState) {
        capture = state
        if case .recording = state {
            meter.start()
        } else {
            meter.stop()
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
}

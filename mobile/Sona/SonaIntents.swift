import AppIntents
import UserNotifications

struct StartDictationIntent: AppIntent {
    static var title: LocalizedStringResource = "Start dictation"
    static var description = IntentDescription("Open Sona and start on-device dictation.")
    static var openAppWhenRun = true

    func perform() async throws -> some IntentResult & OpensIntent {
        .result(opensIntent: OpenURLIntent(DictationLink.url))
    }
}

struct RecordMeetingIntent: AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Record a meeting"
    static var description = IntentDescription("Start recording a meeting with Sona.")
    static var supportedModes: IntentModes = [.background, .foreground(.dynamic)]

    @Dependency private var controls: any PhoneRecordingControls

    @MainActor
    func perform() async throws -> some IntentResult {
        try await startRecording(using: controls)
        return .result()
    }
}

/// The Action Button's question for Sona's agent: speak, keep using the app you are in,
/// and read the answer in a notification. The Mac answers, so the wait runs as a
/// system background task with its own Live Activity.
@available(iOS 27.0, *)
struct AskSonaIntent: AppIntent, LongRunningIntent {
    static let title: LocalizedStringResource = "Ask Sona"
    static let description = IntentDescription("Ask Sona's agent. The answer arrives as a notification.")
    static var supportedModes: IntentModes { .background }

    @Parameter(title: "Question", requestValueDialog: "What do you want to ask Sona?")
    var question: String

    @Dependency private var model: AppModel

    @MainActor
    func perform() async throws -> some IntentResult {
        guard model.isPaired else { throw AskSonaError.notPaired }
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])
        guard let asked = await model.chat.ask(question) else { throw AskSonaError.refused }
        progress.totalUnitCount = 1
        progress.localizedDescription = String(localized: "agent.progress")
        let chat = model.chat
        let window = PhoneAgentChat.pickUpWindow + PhoneAgentChat.answerWindow
        let turn = try await performBackgroundTask { @MainActor in
            await chat.settled(asked.turn, timeout: window)
        }
        progress.completedUnitCount = 1
        if let turn, turn.status == .unanswered,
           let conversation = chat.conversations.first(where: { $0.id == asked.conversation })
        {
            model.notify(conversation, turn)
        }
        return .result()
    }
}

enum AskSonaError: Error, CustomLocalizedStringResourceConvertible {
    case notPaired
    case refused

    var localizedStringResource: LocalizedStringResource {
        switch self {
        case .notPaired: "Pair Sona with your Mac first. Your Mac answers questions for Sona's agent."
        case .refused: "Sona's agent can't take this question. Keep it shorter, and leave out web addresses and file paths."
        }
    }
}

struct SonaShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: StartDictationIntent(), phrases: ["Start dictation with \(.applicationName)"],
                    shortTitle: "Start dictation", systemImageName: "mic")
        AppShortcut(intent: RecordMeetingIntent(), phrases: ["Record a meeting with \(.applicationName)"],
                    shortTitle: "Record a meeting", systemImageName: "waveform")
        AppShortcut(intent: SearchPhoneNotesIntent(), phrases: [
            "Search \(.applicationName)",
            "Search my \(.applicationName) notes",
            "Find a recording in \(.applicationName)",
        ], shortTitle: "Search notes", systemImageName: "magnifyingglass")
        if #available(iOS 27.0, *) {
            AppShortcut(intent: AskSonaIntent(), phrases: [
                "Ask \(.applicationName)",
                "Ask \(.applicationName) a question",
            ], shortTitle: "Ask Sona", systemImageName: "bubble.left.and.text.bubble.right")
        }
    }
}

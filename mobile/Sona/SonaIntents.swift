import AppIntents

struct StartDictationIntent: AppIntent {
    static var title: LocalizedStringResource = "Start dictation"
    static var description = IntentDescription("Open Sona and start on-device dictation.")
    static var openAppWhenRun = true

    func perform() async throws -> some IntentResult & OpensIntent {
        .result(opensIntent: OpenURLIntent(DictationLink.url))
    }
}

struct RecordMeetingIntent: AppIntent {
    static var title: LocalizedStringResource = "Record a meeting"
    static var description = IntentDescription("Open Sona and record a meeting on this iPhone.")
    static var openAppWhenRun = true

    func perform() async throws -> some IntentResult & OpensIntent {
        .result(opensIntent: OpenURLIntent(DictationLink.meetingURL))
    }
}

struct SonaShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: StartDictationIntent(), phrases: ["Start dictation with \(.applicationName)"],
                    shortTitle: "Start dictation", systemImageName: "mic")
        AppShortcut(intent: RecordMeetingIntent(), phrases: ["Record a meeting with \(.applicationName)"],
                    shortTitle: "Record a meeting", systemImageName: "waveform")
    }
}

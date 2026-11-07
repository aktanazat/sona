import AppIntents

/// What the recording intents ask of the app. The app registers its model at launch.
/// The widget extension compiles the intents so its control and Live Activity can name
/// them, and the system runs every one of them in the app.
@MainActor
protocol PhoneRecordingControls: AnyObject, Sendable {
    /// Starts recording when nothing has to be asked first, and says whether one is
    /// recording or starting.
    func recordWithoutAsking() -> Bool
    /// Opens the recording screen and starts there, asking for what is missing.
    func openRecording()
    func stopRecording()
    func bookmarkRecording()
}

extension AppIntent {
    /// Records without opening Sona when it can. Otherwise Sona opens on the recording
    /// screen, where consent and the microphone are asked for.
    @MainActor
    func startRecording(using controls: any PhoneRecordingControls) async throws {
        if controls.recordWithoutAsking() { return }
        if systemContext.currentMode == .background {
            try await continueInForeground(alwaysConfirm: false)
        }
        controls.openRecording()
    }
}

/// The Control Center control: on starts a recording, off stops it.
struct RecordingToggleIntent: SetValueIntent, AudioRecordingIntent, LiveActivityIntent {
    static var title: LocalizedStringResource = "Record a meeting"
    static var isDiscoverable = false
    static var supportedModes: IntentModes = [.background, .foreground(.dynamic)]

    @Parameter(title: "Recording")
    var value: Bool

    @Dependency private var controls: any PhoneRecordingControls

    @MainActor
    func perform() async throws -> some IntentResult {
        if value {
            try await startRecording(using: controls)
        } else {
            controls.stopRecording()
        }
        return .result()
    }
}

/// The Live Activity's Bookmark button. It never opens the app.
struct BookmarkRecordingIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "Bookmark this moment"
    static var isDiscoverable = false

    @Dependency private var controls: any PhoneRecordingControls

    @MainActor
    func perform() async throws -> some IntentResult {
        controls.bookmarkRecording()
        return .result()
    }
}

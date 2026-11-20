import ActivityKit
import AppIntents
import Foundation

/// What the Lock Screen and the Dynamic Island show while Sona keeps the microphone
/// ready for the keyboard.
struct MicrophoneActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        var readyUntil: Date
        var listening: Bool
    }
}

/// What the End button asks of the app. The app registers its model at launch.
@MainActor
protocol PhoneMicrophoneControls: AnyObject, Sendable {
    func endMicrophoneSession()
}

/// The Live Activity's End button. It never opens the app.
struct EndMicrophoneSessionIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "End microphone session"
    static var isDiscoverable = false

    @Dependency private var controls: any PhoneMicrophoneControls

    @MainActor
    func perform() async throws -> some IntentResult {
        controls.endMicrophoneSession()
        return .result()
    }
}

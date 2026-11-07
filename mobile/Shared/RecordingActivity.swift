import ActivityKit
import Foundation
import WidgetKit

/// What the Lock Screen and the Dynamic Island show while the phone records.
struct RecordingActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        var bookmarks: Int
    }

    var startedAt: Date
}

/// Whether the phone is recording, kept where the Control Center control can read it:
/// the control is drawn by the widget extension, which cannot ask the app.
enum RecordingControlState {
    static let kind = "com.aktanazat.sona.mobile.record-meeting"
    private static let key = "sona.recording.active"

    static var isRecording: Bool {
        UserDefaults(suiteName: KeyboardDraftStore.group)?.bool(forKey: key) ?? false
    }

    static func set(_ recording: Bool) {
        UserDefaults(suiteName: KeyboardDraftStore.group)?.set(recording, forKey: key)
        ControlCenter.shared.reloadControls(ofKind: kind)
    }
}

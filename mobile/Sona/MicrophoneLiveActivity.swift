import ActivityKit
import Foundation

/// The one Live Activity for the microphone session the keyboard dictates through.
@MainActor
final class MicrophoneLiveActivity {
    private var activity: Activity<MicrophoneActivityAttributes>?
    private var shown: MicrophoneActivityAttributes.ContentState?

    init() {
        /* The session ends with the process that held the microphone, so an activity
         * found at launch describes a session that no longer exists. */
        for stale in Activity<MicrophoneActivityAttributes>.activities {
            Task { await stale.end(nil, dismissalPolicy: .immediate) }
        }
    }

    /// Shows `state`, or ends the activity when the session is over.
    func show(_ state: MicrophoneActivityAttributes.ContentState?) {
        guard state != shown else { return }
        shown = state
        guard let state else {
            if let activity {
                self.activity = nil
                Task { await activity.end(nil, dismissalPolicy: .immediate) }
            }
            return
        }
        let content = ActivityContent(state: state, staleDate: state.readyUntil)
        if let activity {
            Task { await activity.update(content) }
        } else if ActivityAuthorizationInfo().areActivitiesEnabled {
            activity = try? Activity.request(attributes: MicrophoneActivityAttributes(), content: content)
        }
    }
}

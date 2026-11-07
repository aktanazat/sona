import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

@main
struct SonaWidgets: WidgetBundle {
    var body: some Widget {
        RecordMeetingControl()
        RecordingLiveActivity()
    }
}

/// Control Center and the Action button: one press records without opening Sona, the
/// next one stops.
struct RecordMeetingControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: RecordingControlState.kind, provider: Recording()) { isRecording in
            ControlWidgetToggle(isOn: isRecording, action: RecordingToggleIntent()) {
                Label("control.record", systemImage: "waveform")
            } valueLabel: { isOn in
                isOn ? Text("status.recording") : Text("control.off")
            }
            .tint(Theme.recording)
        }
        .displayName("control.record")
    }

    private struct Recording: ControlValueProvider {
        let previewValue = false

        func currentValue() async throws -> Bool {
            RecordingControlState.isRecording
        }
    }
}

/// The clock and the Bookmark button on the Lock Screen and in the Dynamic Island.
struct RecordingLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: RecordingActivityAttributes.self) { context in
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Label("status.recording", systemImage: "waveform")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.recording)
                    Clock(startedAt: context.attributes.startedAt)
                        .font(.title.weight(.semibold))
                    Bookmarked(count: context.state.bookmarks)
                }
                Spacer()
                BookmarkButton()
            }
            .padding()
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Label("status.recording", systemImage: "waveform")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.recording)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Clock(startedAt: context.attributes.startedAt)
                        .font(.subheadline.weight(.semibold))
                        .multilineTextAlignment(.trailing)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack {
                        Bookmarked(count: context.state.bookmarks)
                        Spacer()
                        BookmarkButton()
                    }
                }
            } compactLeading: {
                Image(systemName: "waveform")
                    .foregroundStyle(Theme.recording)
            } compactTrailing: {
                Clock(startedAt: context.attributes.startedAt)
                    .frame(maxWidth: 52)
            } minimal: {
                Image(systemName: "waveform")
                    .foregroundStyle(Theme.recording)
            }
            .keylineTint(Theme.recording)
        }
    }
}

private struct Clock: View {
    let startedAt: Date

    var body: some View {
        Text(timerInterval: startedAt...Date.distantFuture, countsDown: false)
            .monospacedDigit()
    }
}

private struct Bookmarked: View {
    let count: Int

    var body: some View {
        if count > 0 {
            Text("recording.bookmarked \(count)")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }
}

private struct BookmarkButton: View {
    var body: some View {
        Button(intent: BookmarkRecordingIntent()) {
            Label("recording.bookmark", systemImage: "bookmark")
                .font(.subheadline.weight(.semibold))
        }
        .buttonStyle(.bordered)
        .buttonBorderShape(.capsule)
    }
}

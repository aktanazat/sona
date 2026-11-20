import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

@main
struct SonaWidgets: WidgetBundle {
    var body: some Widget {
        RecordMeetingControl()
        RecordingLiveActivity()
        MicrophoneLiveActivityWidget()
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
                    Status(isStale: context.isStale)
                        .font(.subheadline.weight(.semibold))
                    if !context.isStale {
                        Clock(startedAt: context.attributes.startedAt)
                            .font(.title.weight(.semibold))
                    }
                    Bookmarked(count: context.state.bookmarks)
                }
                Spacer()
                if !context.isStale { BookmarkButton() }
            }
            .padding()
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Status(isStale: context.isStale)
                        .font(.subheadline.weight(.semibold))
                }
                DynamicIslandExpandedRegion(.trailing) {
                    if !context.isStale {
                        Clock(startedAt: context.attributes.startedAt)
                            .font(.subheadline.weight(.semibold))
                            .multilineTextAlignment(.trailing)
                    }
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack {
                        Bookmarked(count: context.state.bookmarks)
                        Spacer()
                        if !context.isStale { BookmarkButton() }
                    }
                }
            } compactLeading: {
                Image(systemName: context.isStale ? "stop.circle" : "waveform")
                    .foregroundStyle(context.isStale ? Color.secondary : Theme.recording)
            } compactTrailing: {
                if !context.isStale {
                    Clock(startedAt: context.attributes.startedAt)
                        .frame(maxWidth: 52)
                }
            } minimal: {
                Image(systemName: "waveform")
                    .foregroundStyle(Theme.recording)
            }
            .keylineTint(Theme.recording)
        }
    }
}

/// A stale activity means the recording stopped renewing it, so it no longer runs.
private struct Status: View {
    let isStale: Bool

    var body: some View {
        if isStale {
            Label("status.stopped", systemImage: "stop.circle")
                .foregroundStyle(.secondary)
        } else {
            Label("status.recording", systemImage: "waveform")
                .foregroundStyle(Theme.recording)
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

/// The microphone the keyboard dictates through, kept ready by Sona, with End.
struct MicrophoneLiveActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: MicrophoneActivityAttributes.self) { context in
            HStack(spacing: 12) {
                MicrophoneSymbol(listening: context.state.listening, isStale: context.isStale)
                    .font(.title3.weight(.semibold))
                    .frame(width: 44, height: 44)
                    .background(Theme.accent.opacity(context.isStale ? 0 : 0.16), in: Circle())
                VStack(alignment: .leading, spacing: 2) {
                    MicrophoneStatus(listening: context.state.listening, isStale: context.isStale)
                        .font(.headline)
                    if !context.isStale {
                        MicrophoneCountdown(until: context.state.readyUntil)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 8)
                if !context.isStale { EndMicrophoneButton() }
            }
            .padding()
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    MicrophoneStatus(listening: context.state.listening, isStale: context.isStale)
                        .font(.subheadline.weight(.semibold))
                        .padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    if !context.isStale {
                        Text(timerInterval: Date()...max(Date(), context.state.readyUntil), countsDown: true)
                            .font(.subheadline.weight(.semibold))
                            .monospacedDigit()
                            .multilineTextAlignment(.trailing)
                            .padding(.trailing, 4)
                    }
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack {
                        Text("microphone.forKeyboard")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                        Spacer()
                        if !context.isStale { EndMicrophoneButton() }
                    }
                    .padding(.horizontal, 4)
                }
            } compactLeading: {
                MicrophoneSymbol(listening: context.state.listening, isStale: context.isStale)
            } compactTrailing: {
                if !context.isStale {
                    Text(timerInterval: Date()...max(Date(), context.state.readyUntil), countsDown: true)
                        .monospacedDigit()
                        .frame(maxWidth: 44)
                }
            } minimal: {
                MicrophoneSymbol(listening: context.state.listening, isStale: context.isStale)
            }
            .keylineTint(Theme.accent)
        }
    }
}

private struct MicrophoneSymbol: View {
    let listening: Bool
    let isStale: Bool

    var body: some View {
        Image(systemName: isStale ? "mic.slash" : listening ? "waveform" : "mic.fill")
            .foregroundStyle(isStale ? Color.secondary : Theme.accent)
            .contentTransition(.symbolEffect(.replace))
            .accessibilityHidden(true)
    }
}

private struct MicrophoneStatus: View {
    let listening: Bool
    let isStale: Bool

    var body: some View {
        if isStale {
            Text("microphone.off").foregroundStyle(.secondary)
        } else if listening {
            Text("microphone.listening")
        } else {
            Text("microphone.ready")
        }
    }
}

private struct MicrophoneCountdown: View {
    let until: Date

    var body: some View {
        Text("microphone.endsIn \(Text(timerInterval: Date()...max(Date(), until), countsDown: true))")
            .monospacedDigit()
    }
}

private struct EndMicrophoneButton: View {
    var body: some View {
        Button(intent: EndMicrophoneSessionIntent()) {
            Text("microphone.end")
                .font(.subheadline.weight(.semibold))
        }
        .buttonStyle(.bordered)
        .buttonBorderShape(.capsule)
        .tint(Theme.accent)
    }
}

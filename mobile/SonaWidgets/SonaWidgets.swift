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
        TodayWidget()
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
                        .lineLimit(1)
                        .padding(.leading, 4)
                        .dynamicIsland(verticalPlacement: .belowIfTooWide)
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

/// Today on the Home Screen and the Lock Screen: what the Mac says needs you, as of the
/// app's last look at the vault. Task titles are hidden while the iPhone is locked.
struct TodayWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WorkTodayGlance.widgetKind, provider: TodayProvider()) { entry in
            TodayWidgetView(glance: entry.glance)
                .containerBackground(.fill.tertiary, for: .widget)
                .widgetURL(WorkTodayGlance.url)
        }
        .configurationDisplayName(Text("today.title"))
        .description(Text("widget.today.description"))
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular, .accessoryCircular, .accessoryInline])
    }
}

struct TodayGlanceEntry: TimelineEntry {
    let date: Date
    let glance: WorkTodayGlance?
}

/// The app reloads the timeline whenever the Mac's Today changes, so one entry is enough.
struct TodayProvider: TimelineProvider {
    private static let sample = WorkTodayGlance(
        producedAt: .now, dueTitles: ["Send the deck", "Book the room"], dueCount: 2,
        triageCount: 1, questionCount: 1, inboxCount: 3
    )

    func placeholder(in context: Context) -> TodayGlanceEntry {
        TodayGlanceEntry(date: .now, glance: Self.sample)
    }

    func getSnapshot(in context: Context, completion: @escaping (TodayGlanceEntry) -> Void) {
        completion(TodayGlanceEntry(date: .now, glance: WorkTodayGlance.load() ?? (context.isPreview ? Self.sample : nil)))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<TodayGlanceEntry>) -> Void) {
        completion(Timeline(entries: [TodayGlanceEntry(date: .now, glance: WorkTodayGlance.load())], policy: .never))
    }
}

struct TodayWidgetView: View {
    let glance: WorkTodayGlance?
    @Environment(\.widgetFamily) private var family

    var body: some View {
        if let glance {
            switch family {
            case .accessoryCircular:
                ZStack {
                    AccessoryWidgetBackground()
                    VStack(spacing: 0) {
                        Image(systemName: "sun.max")
                            .font(.caption)
                        Text(verbatim: "\(glance.decisions + glance.dueCount)")
                            .font(.title3.weight(.semibold))
                            .monospacedDigit()
                    }
                }
                .accessibilityElement(children: .combine)
            case .accessoryInline:
                Label {
                    Text(counts(glance))
                } icon: {
                    Image(systemName: "sun.max")
                }
            case .accessoryRectangular:
                VStack(alignment: .leading, spacing: 1) {
                    Text("today.title").font(.headline)
                    Text(counts(glance)).font(.caption)
                    if let first = glance.dueTitles.first {
                        Text(verbatim: first)
                            .font(.caption)
                            .lineLimit(1)
                            .privacySensitive()
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            default:
                VStack(alignment: .leading, spacing: 6) {
                    Label("today.title", systemImage: "sun.max")
                        .font(.headline)
                        .foregroundStyle(Theme.accent)
                    Text(counts(glance))
                        .font(.subheadline.weight(.semibold))
                    ForEach(Array(glance.dueTitles.prefix(family == .systemSmall ? 2 : WorkTodayGlance.shownTitles).enumerated()), id: \.offset) { _, title in
                        Text(verbatim: title)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .privacySensitive()
                    }
                    Spacer(minLength: 0)
                    Text(glance.producedAt, style: .time)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        } else {
            Text("widget.today.empty")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
    }

    private func counts(_ glance: WorkTodayGlance) -> String {
        [
            String(format: NSLocalizedString("widget.today.decide %lld", comment: ""), Int64(glance.decisions)),
            String(format: NSLocalizedString("widget.today.due %lld", comment: ""), Int64(glance.dueCount)),
        ].joined(separator: " · ")
    }
}

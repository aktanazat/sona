import SwiftUI
import WidgetKit

@main
struct SonaWatchWidgets: WidgetBundle {
    var body: some Widget {
        RecordComplication()
    }
}

/// Sona on the watch face: one tap opens Sona and starts recording.
struct RecordComplication: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "com.aktanazat.sona.mobile.watchkitapp.record", provider: RecordProvider()) { _ in
            RecordComplicationView()
                .containerBackground(.fill.tertiary, for: .widget)
                .widgetURL(WatchLink.record)
        }
        .configurationDisplayName(Text("watch.complication"))
        .description(Text("watch.complication.description"))
        .supportedFamilies([.accessoryCircular, .accessoryCorner, .accessoryInline, .accessoryRectangular])
    }
}

/// Nothing in the complication changes, so it has one entry for good.
struct RecordProvider: TimelineProvider {
    func placeholder(in context: Context) -> RecordEntry { RecordEntry(date: .now) }

    func getSnapshot(in context: Context, completion: @escaping (RecordEntry) -> Void) {
        completion(RecordEntry(date: .now))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<RecordEntry>) -> Void) {
        completion(Timeline(entries: [RecordEntry(date: .now)], policy: .never))
    }
}

struct RecordEntry: TimelineEntry {
    let date: Date
}

struct RecordComplicationView: View {
    @Environment(\.widgetFamily) private var family

    var body: some View {
        switch family {
        case .accessoryCorner:
            Image(systemName: "mic.fill")
                .font(.title3)
                .widgetLabel("watch.record")
        case .accessoryInline:
            Label("watch.record", systemImage: "mic.fill")
        case .accessoryRectangular:
            HStack(spacing: 8) {
                Image(systemName: "mic.fill")
                    .font(.title3)
                    .foregroundStyle(Theme.recording)
                Text("watch.complication.record")
                    .font(.headline)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        default:
            ZStack {
                AccessoryWidgetBackground()
                Image(systemName: "mic.fill")
                    .font(.title3)
                    .foregroundStyle(Theme.recording)
            }
            .accessibilityLabel(Text("a11y.watch.record"))
        }
    }
}

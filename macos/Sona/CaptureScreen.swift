import SwiftUI

/// The first page: what the microphone is doing, the history in three numbers,
/// what needs a decision, what happened lately.
struct CaptureScreen: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Page {
            hero.padding(.bottom, 32)
            if let error = model.coreError {
                Card {
                    CardRow {
                        Text(error).bodyText(15, Theme.inkSecondary)
                    }
                }
                .padding(.bottom, 32)
            }
            PageSection("All time") {
                Card {
                    HStack(spacing: 0) {
                        Stat(label: "Dictations", value: "\(model.stats?.entries ?? 0)")
                        Rectangle().fill(Theme.hairline).frame(width: 1)
                        Stat(label: "Words", value: (model.stats?.totalWords ?? 0).formatted())
                        Rectangle().fill(Theme.hairline).frame(width: 1)
                        Stat(label: "Meetings", value: "\(SampleData.meetings.count)")
                    }
                }
            }
            HStack(alignment: .top, spacing: 24) {
                if !model.decisions.isEmpty {
                    PageSection("Needs you") {
                        Card {
                            ForEach(model.decisions) { decision in
                                DecisionRow(decision: decision)
                            }
                        }
                    }
                }
                PageSection("Recent") {
                    Card {
                        ForEach(recent) { item in
                            RecentRow(item: item)
                        }
                    }
                }
            }
        }
    }

    /// The old "Ready" card: one big word, the shortcut, one action.
    private var hero: some View {
        Card {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    switch model.capture {
                    case .idle:
                        Text("Ready").heroText()
                    case let .recording(since):
                        TimelineView(.periodic(from: since, by: 1)) { context in
                            Text("Recording \(context.date.timeIntervalSince(since).clock)")
                                .heroText()
                                .monospacedDigit()
                        }
                        Circle().fill(Theme.live).frame(width: 10, height: 10)
                    case let .working(kind):
                        Text(kind.capitalized).heroText()
                    }
                }
                HStack(spacing: 8) {
                    Button(action: model.beginShortcutCapture) {
                        Shortcut(model.pushToTalk)
                    }
                    .buttonStyle(.plain)
                    .disabled(model.capture != .idle)
                    .accessibilityLabel("Change recording shortcut")
                    .help("Change recording shortcut")
                    Text(hint).bodyText(15, Theme.inkSecondary)
                }
                if !model.liveText.isEmpty, model.capture != .idle {
                    Text(model.liveText)
                        .bodyText(16)
                        .lineLimit(3)
                        .frame(maxWidth: 640, alignment: .leading)
                }
                HStack(spacing: 10) {
                    Button {
                        model.toggleCapture()
                    } label: {
                        Label(model.capture == .idle ? "Start recording" : "Stop", systemImage: model.capture == .idle ? "mic" : "stop.fill")
                    }
                    .buttonStyle(.primary)
                    if model.capture == .idle {
                        Button {
                        } label: {
                            Label("Import audio", systemImage: "waveform.badge.plus")
                        }
                        .buttonStyle(.secondary)
                    } else {
                        Button("Cancel") { model.cancelCapture() }.buttonStyle(.secondary)
                    }
                }
                .padding(.top, 6)
            }
            .padding(24)
        }
    }

    private var hint: String {
        switch model.capture {
        case .idle: "tap to toggle · hold to talk · \(model.activeModel?.name ?? "No model")"
        case .recording: "release to paste · \(model.inputDevice)"
        case .working: "the words land in the app in front"
        }
    }

    private var recent: [RecentItem] {
        let meetings = SampleData.meetings.prefix(3).map(RecentItem.meeting)
        let words = model.transcriptions.prefix(4).map(RecentItem.transcription)
        return (meetings + words).sorted { $0.date > $1.date }
    }
}

enum RecentItem: Identifiable {
    case meeting(Meeting)
    case transcription(Transcription)

    var id: String {
        switch self {
        case let .meeting(meeting): "m\(meeting.id)"
        case let .transcription(transcription): "t\(transcription.id)"
        }
    }

    var date: Date {
        switch self {
        case let .meeting(meeting): meeting.date
        case let .transcription(transcription): transcription.date
        }
    }
}

struct RecentRow: View {
    @Environment(AppModel.self) private var model
    let item: RecentItem

    var body: some View {
        switch item {
        case let .meeting(meeting):
            CardRow {
                model.place = .meetings
                model.selectedMeeting = meeting
            } leading: {
                VStack(alignment: .leading, spacing: 4) {
                    Text(meeting.title).bodyText(16)
                    Text("\(meeting.app), \(meeting.date.time) · \(meeting.date.relativeDay.lowercased())").metaText()
                }
            }
        case let .transcription(transcription):
            CardRow {
                model.place = .library
                model.selectedTranscription = transcription
            } leading: {
                VStack(alignment: .leading, spacing: 4) {
                    Text(transcription.text).bodyText(16).lineLimit(2)
                    Text("\(transcription.words) words, \(transcription.date.time) · \(transcription.date.relativeDay.lowercased())").metaText()
                }
            }
        }
    }
}

/// One decision Sona could not make alone, with its two answers underneath.
struct DecisionRow: View {
    @Environment(AppModel.self) private var model
    let decision: Decision

    var body: some View {
        CardRow {
            VStack(alignment: .leading, spacing: 10) {
                Text(decision.text).bodyText(16)
                HStack(spacing: 8) {
                    Button(decision.accept) { model.resolve(decision) }.buttonStyle(.compact)
                    Button(decision.decline) { model.resolve(decision) }.buttonStyle(.compact)
                }
            }
        }
    }
}

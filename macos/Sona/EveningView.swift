import SwiftUI

/// Home's end of the day: today's lines once they are written, the button
/// that writes them once the evening digest's time has passed, and the
/// recordings Sona titled itself with where they may belong.
struct EveningView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.evening
        if let evening = store.evening {
            VStack(alignment: .leading, spacing: 0) {
                TimelineView(.everyMinute) { context in
                    EveningLinesSection(
                        store: store, evening: evening,
                        due: due(context.date))
                }
                if !evening.unfiled.isEmpty {
                    PageSection("Not filed") {
                        Card {
                            ForEach(evening.unfiled) { recording in
                                UnfiledRows(store: store, recording: recording)
                            }
                        }
                    }
                    .padding(.bottom, 20)
                }
            }
        }
    }

    /// Past the evening digest's time today.
    private func due(_ now: Date) -> Bool {
        let parts = Calendar.current.dateComponents([.hour, .minute], from: now)
        let minute = (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
        return minute >= model.meetingSettings.settings.digestMinuteOfDay
    }
}

private struct EveningLinesSection: View {
    let store: EveningStore
    let evening: HomeEvening
    let due: Bool

    var body: some View {
        if let lines = evening.lines {
            PageSection("End of day") {
                Card {
                    ForEach(Array(lines.lines.enumerated()), id: \.offset) { _, line in
                        CardRow { Text(line).bodyText(14) }
                    }
                    CardRow {
                        Text("Written \(FeedClock.date(lines.writtenAtUtcMs).time)").metaText()
                    } trailing: {
                        writeButton("Rewrite")
                    }
                }
                ErrorNote(store.error)
            }
            .padding(.bottom, 20)
        } else if evening.hasActivity, due || store.writing {
            PageSection("End of day") {
                Card {
                    CardRow {
                        Text(store.writing ? "Summing up today…" : "Today in five lines")
                            .metaText(Theme.inkSecondary)
                    } trailing: {
                        writeButton("Sum up today")
                    }
                }
                ErrorNote(store.error)
            }
            .padding(.bottom, 20)
        }
    }

    private func writeButton(_ title: String) -> some View {
        Button(title, action: store.write)
            .buttonStyle(.compact)
            .disabled(store.writing)
    }
}

/// One recording and each place it may belong, with why.
private struct UnfiledRows: View {
    @Environment(AppModel.self) private var model
    let store: EveningStore
    let recording: UnfiledRecording

    var body: some View {
        CardRow(action: { model.openMeeting(recording.sessionId) }) {
            VStack(alignment: .leading, spacing: 4) {
                Text(recording.title).bodyText(14)
                Text(when).metaText()
            }
        }
        ForEach(recording.suggestions) { suggestion in
            CardRow {
                VStack(alignment: .leading, spacing: 4) {
                    Text(suggestion.target.headline).bodyText(14)
                    FilingReasonsLine(sessionId: recording.sessionId, reasons: suggestion.reasons)
                }
            } trailing: {
                HStack(spacing: 12) {
                    Button("File") { store.decide(suggestion, accept: true) }
                        .buttonStyle(.compact)
                    Button("Not this") { store.decide(suggestion, accept: false) }
                        .buttonStyle(.quiet)
                }
                .disabled(store.deciding.contains(suggestion.id))
            }
        }
    }

    private var when: String {
        let start = FeedClock.date(recording.startedAtUtcMs)
        return "\(start.relativeDay) \(start.time)"
    }
}

/// Why a maybe was offered, in facts he can check; a phrase opens where it
/// was said.
struct FilingReasonsLine: View {
    @Environment(AppModel.self) private var model
    let sessionId: MeetingSessionId
    let reasons: [FilingReason]
    /// Shows the running capture's words at an offset, on the live screen.
    var showLive: ((Int64) -> Void)?

    var body: some View {
        HStack(spacing: 6) {
            ForEach(Array(reasons.enumerated()), id: \.offset) { index, reason in
                if index > 0 { Text("·").metaText() }
                reasonView(reason)
            }
        }
    }

    @ViewBuilder
    private func reasonView(_ reason: FilingReason) -> some View {
        switch reason {
        case let .onCalendar(overlaps):
            Text(overlaps ? "on your calendar then" : "on your calendar just before or after").metaText()
        case let .voiceMatches(name, startOffsetNs):
            if let startOffsetNs, let showLive {
                Button {
                    showLive(startOffsetNs)
                } label: {
                    Text("\(name)'s voice at \(startOffsetNs.meetingOffsetClock) ▶")
                        .metaText(Theme.inkSecondary)
                }
                .buttonStyle(.plain)
            } else {
                Text("a voice matches \(name)").metaText()
            }
        case let .said(phrase, count, segmentId):
            Button {
                model.meetings.open(sessionId, segmentId: segmentId)
                model.go(.meetings)
            } label: {
                Text("“\(phrase)” said \(count == 1 ? "once" : "\(count) times") ▶")
                    .metaText(Theme.inkSecondary)
            }
            .buttonStyle(.plain)
        case .onlyMicrophone:
            Text("only the microphone heard voices").metaText()
        case let .appAudio(bundleId):
            Text("voices came through \(InstalledApp.find(bundleId)?.name ?? bundleId)").metaText()
        case .inInvite:
            Text("from your calendar").metaText()
        case let .planned(plannedIn, saidAtUtcMs, quote, citation):
            Button {
                model.openPast(plannedIn, segmentId: citation?.segmentId)
            } label: {
                Text(quote.map { "said \(FeedClock.ago(saidAtUtcMs)): “\($0)” ▶" }
                    ?? "planned \(FeedClock.ago(saidAtUtcMs)) ▶")
                    .metaText(Theme.inkSecondary)
                    .lineLimit(2)
            }
            .buttonStyle(.plain)
        }
    }
}

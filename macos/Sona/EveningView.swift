import SwiftUI

/// Home's end of the day: today's lines once they are written, the button
/// that writes them once the evening digest's time has passed, the work I
/// kept and let slip, and the recordings Sona titled itself with where they
/// may belong.
struct EveningView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.evening
        VStack(alignment: .leading, spacing: 0) {
            if let evening = store.evening {
                TimelineView(.everyMinute) { context in
                    EveningLinesSection(
                        store: store, evening: evening,
                        due: model.eveningDue(context.date))
                }
            }
            TimelineView(.everyMinute) { context in
                if let shutdown = model.shutdown(at: context.date) {
                    ShutdownSection(shutdown: shutdown, now: TaskDates.milliseconds(context.date))
                }
            }
            if let evening = store.evening, !evening.unfiled.isEmpty {
                PageSection("Not filed") {
                    Card {
                        ForEach(evening.unfiled) { recording in
                            UnfiledRows(store: store, recording: recording)
                        }
                    }
                }
            }
        }
    }
}

/// Kept, then slipped, each slipped task with where it goes next, then
/// what I said I'd do tomorrow.
private struct ShutdownSection: View {
    @Environment(AppModel.self) private var model
    let shutdown: Shutdown
    let now: Int64

    var body: some View {
        PageSection("Shutdown") {
            Card {
                if !shutdown.kept.isEmpty {
                    label("Kept", count: shutdown.kept.count)
                    ForEach(shutdown.kept) { row in
                        HStack(spacing: 10) {
                            TaskStatusIcon(status: row.status)
                            Text(row.title).bodyText(14, Theme.inkSecondary).lineLimit(1)
                        }
                        .padding(.horizontal, 16)
                        .frame(maxWidth: .infinity, minHeight: Theme.listRow, alignment: .leading)
                        .accessibilityElement(children: .combine)
                    }
                }
                if !shutdown.slipped.isEmpty {
                    label("Slipped", count: shutdown.slipped.count)
                    ForEach(shutdown.slipped) { row in
                        SlippedRow(row: row, highlighted: model.rituals.slipped == row.id, now: now)
                    }
                }
                if !shutdown.tomorrow.isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text("Tomorrow").font(TypeScale.label(13)).foregroundStyle(Theme.inkSecondary)
                        Text(tomorrow).bodyText(14).lineLimit(1)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .overlay(alignment: .top) { Hairline().padding(.horizontal, 16) }
                }
            }
        }
    }

    /// Three titles, then how many more.
    private var tomorrow: String {
        let titles = shutdown.tomorrow
        let shown = titles.prefix(3).joined(separator: " · ")
        return titles.count > 3 ? "\(shown) +\(titles.count - 3)" : shown
    }

    private func label(_ title: String, count: Int) -> some View {
        HStack(spacing: 6) {
            Text(title).foregroundStyle(Theme.inkSecondary)
            Text(verbatim: "\(count)").monospacedDigit().foregroundStyle(Theme.inkTertiary)
        }
        .font(TypeScale.label(12))
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 4)
    }
}

/// A slipped task; pointed at, it offers its three moves with their keys.
private struct SlippedRow: View {
    @Environment(AppModel.self) private var model
    let row: WorkItemRow
    let highlighted: Bool
    let now: Int64

    var body: some View {
        HStack(spacing: 10) {
            TaskStatusIcon(status: row.status)
            Text(row.title).bodyText(14).lineLimit(1)
            Spacer(minLength: 12)
            if highlighted {
                HStack(spacing: 14) {
                    move("Tomorrow", key: "right", SlipMove.tomorrow)
                    move("This week", key: "w", SlipMove.thisWeek)
                    move("Drop", key: "x", SlipMove.drop)
                }
                .transition(.opacity)
            } else if let due = row.dueAtMs {
                TaskDue(due: due, open: true, now: now)
            }
        }
        .padding(.horizontal, 16)
        .frame(maxWidth: .infinity, minHeight: Theme.listRow, alignment: .leading)
        .background(highlighted ? Theme.wash : .clear)
        .hoverFade(highlighted)
        .contentShape(Rectangle())
        .onHover { inside in
            if inside { model.rituals.slipped = row.id }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(row.title)
    }

    private func move(_ title: String, key: String, _ patch: WorkPatch) -> some View {
        Button {
            model.moveSlipped(row.id, patch)
        } label: {
            HStack(spacing: 6) {
                Text(title)
                Shortcut(key)
            }
        }
        .buttonStyle(QuietButton(compact: true))
        .accessibilityLabel(title)
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
    /// Set only on the live screen, while the recording is made: shows its
    /// words at an offset. A plan's ▶ waits until the recording ends, so the
    /// live screen never closes from here.
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
        case let .callLists(name):
            Text("the call lists \(name)").metaText()
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
            let said = quote.map { "said \(FeedClock.ago(saidAtUtcMs)): “\($0)”" }
                ?? "planned \(FeedClock.ago(saidAtUtcMs))"
            if showLive == nil {
                Button {
                    model.openPast(plannedIn, segmentId: citation?.segmentId)
                } label: {
                    Text("\(said) ▶").metaText(Theme.inkSecondary).lineLimit(2)
                }
                .buttonStyle(.plain)
            } else {
                Text(said).metaText().lineLimit(2)
            }
        }
    }
}

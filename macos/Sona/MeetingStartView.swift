import AppKit
import SwiftUI

/// Everything that leads to a recording: what Sona noticed, what the calendar
/// says is next, the meeting that was interrupted, and the gate that stands
/// between a press and a capture. The shell lays these out on the meetings
/// page above the list of recordings.
/// `MeetingsHome.tsx`, `MeetingStartGate.tsx`, `MeetingPreviewCard.tsx`,
/// `MeetingSuggestionPreviews.tsx`, `PreMeetingCountdownCard.tsx`.

// MARK: - The gate

/// The one screen between a press and a capture. What will be recorded, one
/// word per source, and the assurance sentence the press acknowledges: the
/// click on Record below that line is the consent act, so nothing else on the
/// screen offers a second affirmative.
struct MeetingStartGateView: View {
    let store: MeetingLiveStore

    var body: some View {
        Page {
            if let session = store.gate {
                VStack(alignment: .leading, spacing: 12) {
                    BackLink(title: "Back") { store.cancel() }
                    Text(
                        store.gateBlocked || !store.canStart
                            ? "Recording did not start" : "Ready to record"
                    )
                    .titleText()
                }
                .padding(.bottom, 28)

                if let facts = store.options?.preview {
                    MeetingStartPreviewCard(
                        facts: facts,
                        armed: store.options?.sources ?? [],
                        notesTemplate: store.notesTemplate,
                        expanded: true)
                        .padding(.bottom, 32)
                }

                PageSection("What this will record") {
                    MeetingSourceListView(readings: store.readings(session))
                }

                if session.storage != .available {
                    Text("Encrypted meeting storage is unavailable.")
                        .bodyText(14, Theme.live)
                        .padding(.bottom, 16)
                }

                ErrorNote(store.error)

                VStack(alignment: .leading, spacing: 16) {
                    Text("Records this Mac's audio locally. Nothing joins the call.")
                        .bodyText(14)

                    if store.gateBlocked {
                        Toggle(
                            "The meeting is marked partial, and the missing source is named in it.",
                            isOn: Binding(
                                get: { store.acceptPartial },
                                set: { store.setAcceptPartial($0) })
                        )
                        .toggleStyle(.checkbox)
                        .font(TypeScale.body(14))
                        .disabled(store.starting)
                    }

                    if !store.canStart {
                        Text("This action is not available in the current phase.")
                            .bodyText(14, Theme.live)
                    }

                    HStack {
                        Spacer()
                        Button(store.refreshing ? "Checking…" : "Refresh") { store.refresh() }
                            .buttonStyle(.secondary)
                            .disabled(store.refreshing || store.starting || !store.canRefresh)
                        Button(startWord) { store.record() }
                            .buttonStyle(.primary)
                            .disabled(
                                store.starting || !store.canStart
                                    || (store.gateBlocked && !store.acceptPartial))
                    }
                }
            }
        }
    }

    private var startWord: String {
        if store.starting { return "Starting…" }
        return store.gateBlocked ? "Record without it" : "Record"
    }
}

// MARK: - One word per source

/// What each lane is doing, flat on a hairline. Rows, not tiles: two cards
/// side by side implied a comparison that does not exist.
struct MeetingSourceListView: View {
    let readings: [MeetingSourceReading]

    var body: some View {
        Card {
            ForEach(Array(readings.enumerated()), id: \.offset) { _, reading in
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 8) {
                            Text(reading.name).bodyText(14)
                            if reading.required { Chip("Required") }
                        }
                        if reading.missingAudio {
                            Text("Some of this audio is missing.")
                                .metaText(Theme.live)
                        }
                    }
                } trailing: {
                    Text(reading.word)
                        .metaText(reading.urgent ? Theme.live : Theme.inkSecondary)
                }
            }
        }
    }
}

// MARK: - What is about to be recorded

/// The facts behind one offer, in one card: when, where from, who, and what
/// will be kept. The card carries no Start of its own — the screen that shows
/// it owns the press.
struct MeetingStartPreviewCard: View {
    let facts: MeetingStartFacts
    var armed: [MeetingSourceKind] = []
    var notesTemplate: MeetingNotesTemplate?
    /// True when the template came from the series rather than the app.
    var templateFromSeries = false
    var expanded = false
    /// The two presses a list row offers. Unwired, neither is drawn.
    var onRecord: (() -> Void)?
    var onSkip: (() -> Void)?
    var notify: String?

    @State private var open = false
    @State private var showAll = false
    @State private var linkFailed = false

    var body: some View {
        Card {
            CardRow(action: { open.toggle() }) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(facts.heading).bodyText(15)
                    if let line = summary {
                        Text(line).metaText()
                    }
                }
            } trailing: {
                HStack(spacing: 8) {
                    if let onSkip {
                        Button("Skip", action: onSkip).buttonStyle(.quiet)
                    }
                    if let onRecord {
                        Button("Record", action: onRecord).buttonStyle(.secondary)
                    }
                    Image(systemName: open || expanded ? "chevron.up" : "chevron.down")
                        .foregroundStyle(Theme.inkTertiary)
                        .font(.system(size: 11, weight: .semibold))
                }
            }
            if open || expanded {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        CardRow {
                            Text(row.0).metaText()
                        } trailing: {
                            Text(row.1).bodyText(13).multilineTextAlignment(.trailing)
                        }
                    }
                    if !facts.participants.isEmpty {
                        participants
                    }
                    if let url = facts.url, !url.isEmpty {
                        CardRow {
                            Text("Link").metaText()
                        } trailing: {
                            VStack(alignment: .trailing, spacing: 2) {
                                Button(url) { open(url) }
                                    .buttonStyle(.quiet)
                                if linkFailed {
                                    Text("Sona could not open that link.")
                                        .metaText(Theme.live)
                                }
                            }
                        }
                    }
                    if let description = facts.description, !description.isEmpty {
                        CardRow {
                            Text("Description").metaText()
                        } trailing: {
                            VStack(alignment: .trailing, spacing: 4) {
                                Text(description)
                                    .bodyText(13, Theme.inkSecondary)
                                    .lineLimit(showAll ? nil : 3)
                                    .multilineTextAlignment(.trailing)
                                if description.count > 160 {
                                    Button(showAll ? "Less" : "More") { showAll.toggle() }
                                        .buttonStyle(.quiet)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The one line under the title: when it is, and how many people.
    private var summary: String? {
        var parts: [String] = []
        if let start = facts.startUtcMs?.meetingDate {
            parts.append("\(start.relativeDay), \(start.time)")
        }
        if let count = facts.attendeeCount, count > 0 {
            parts.append(count == 1 ? "1 person" : "\(count) people")
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// Every fact the offer actually carries. A calendar that left a field
    /// empty gets no row for it.
    private var rows: [(String, String)] {
        var out: [(String, String)] = []
        if let start = facts.startUtcMs?.meetingDate {
            var when = "\(start.relativeDay), \(start.time)"
            if let seconds = facts.durationSeconds, seconds > 0 {
                when += " · \(seconds.spoken)"
            }
            out.append(("Time", when))
        }
        if let calendar = facts.calendarName, !calendar.isEmpty {
            out.append(("Calendar", calendar))
        }
        if let app = facts.appName, !app.isEmpty {
            out.append(("App", app))
        }
        if let notify {
            out.append(("Notify", notify))
        }
        out.append(("Recording", armed.isEmpty
            ? "No source chosen"
            : armed.map(\.label).joined(separator: " · ")))
        if let notesTemplate {
            out.append((
                "Notes",
                templateFromSeries
                    ? "\(notesTemplate.label) for this series" : notesTemplate.label))
        } else {
            out.append(("Notes", "App default"))
        }
        return out
    }

    /// Who is coming, counted by their answer. Names Sona has are listed; the
    /// rest are a number, because a list of blanks is not a list.
    private var participants: some View {
        CardRow {
            Text("Participants").metaText()
        } trailing: {
            VStack(alignment: .trailing, spacing: 2) {
                ForEach(tallies, id: \.0) { tally in
                    Text("\(tally.0) \(tally.1)").bodyText(13, Theme.inkSecondary)
                }
                if unnamed > 0 {
                    Text("\(unnamed) more, not named").metaText()
                }
            }
        }
    }

    private var tallies: [(String, Int)] {
        var counts: [String: Int] = [:]
        var order: [String] = []
        for person in facts.participants {
            let label = person.isSelf ? "You" : person.status.label
            if counts[label] == nil { order.append(label) }
            counts[label, default: 0] += 1
        }
        return order.map { ($0, counts[$0] ?? 0) }
    }

    private var unnamed: Int {
        max(0, (facts.attendeeCount ?? 0) - facts.participants.count)
    }

    private func open(_ url: String) {
        guard let target = URL(string: url), NSWorkspace.shared.open(target) else {
            linkFailed = true
            return
        }
        linkFailed = false
    }
}

// MARK: - What Sona noticed

/// A meeting app that looks busy. The offer is a preview, not a claim: Sona
/// saw an app, so the card says which app and offers the press.
struct MeetingSuggestionsView: View {
    let store: MeetingLiveStore

    var body: some View {
        if !store.offered.isEmpty {
            PageSection("Meeting detected") {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(store.offered) { suggestion in
                        MeetingStartPreviewCard(
                            facts: MeetingStartFacts(suggestion: suggestion),
                            armed: MeetingSourceKind.allCases,
                            notesTemplate: store.notesTemplate,
                            onRecord: { store.start(suggestion) },
                            onSkip: { store.skip(suggestion) })
                    }
                }
            }
        }
    }
}

// MARK: - The meeting that is about to start

/// The calendar event the clock is running down to, with the relationship line
/// behind it. `PreMeetingCountdownCard.tsx`.
struct MeetingStartCountdownView: View {
    let store: MeetingLiveStore

    var body: some View {
        if let countdown = store.countdown {
            PageSection("Starting soon") {
                VStack(alignment: .leading, spacing: 12) {
                    HStack(spacing: 8) {
                        Chip("Starts in \(max(0, countdown.secondsToStart))s")
                        if let brief = store.seriesBrief {
                            Text(brief).metaText()
                        }
                    }
                    MeetingStartPreviewCard(
                        facts: MeetingStartFacts(event: countdown.event),
                        armed: MeetingSourceKind.allCases,
                        notesTemplate: store.notesTemplate,
                        expanded: true,
                        notify: notify)
                    // Opening the meeting when it starts is a detection
                    // setting, and writing it is the Settings slice's command,
                    // so the state reads here and is changed there.
                    Text("Open this meeting when it starts")
                        .metaText(
                            store.detection?.settings.autoStartOnOpenPane == true
                                ? Theme.ink : Theme.inkTertiary)
                    if let prompt = store.prompts.last {
                        HStack(spacing: 8) {
                            Button("Start transcribing") { store.record(prompt) }
                                .buttonStyle(.primary)
                                .disabled(store.pending != nil)
                            Button("Dismiss") { store.answer(prompt, accepted: false) }
                                .buttonStyle(.secondary)
                                .disabled(store.pending != nil)
                        }
                    } else {
                        Button("Record") { store.start(countdown.event) }
                            .buttonStyle(.primary)
                            .disabled(store.pending != nil || store.starting)
                    }
                }
            }
        }
    }

    private var notify: String {
        guard let access = store.detection?.notificationAccess else { return "" }
        return access == .authorized
            ? "Notifies you at the start"
            : "Shown in Sona only: notifications are off"
    }
}

// MARK: - The meeting that was interrupted

/// A capture that never got its ending: Sona quit, the Mac slept, the process
/// died. Finishing it seals what was recorded; discarding it deletes it.
struct MeetingRecoveryView: View {
    let store: MeetingLiveStore
    var onOpenSession: (MeetingSessionId) -> Void = { _ in }

    var body: some View {
        if !store.recovery.isEmpty {
            PageSection("Unfinished meetings") {
                Card {
                    ForEach(store.recovery) { entry in
                        CardRow(action: { onOpenSession(entry.sessionId) }) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(entry.title).bodyText(14)
                                Text(line(entry)).metaText()
                            }
                        } trailing: {
                            HStack(spacing: 8) {
                                Button("Discard") { store.discardRecovery(entry) }
                                    .buttonStyle(.quiet)
                                    .disabled(store.pending != nil)
                                Button("Finish") { store.finalizeRecovery(entry) }
                                    .buttonStyle(.secondary)
                                    .disabled(store.pending != nil)
                            }
                        }
                    }
                }
            }
        }
    }

    private func line(_ entry: MeetingHistorySummary) -> String {
        var parts = ["\(entry.date.relativeDay), \(entry.date.time)"]
        if let recorded = entry.recordedDuration, recorded > 0 {
            parts.append(recorded.clock)
        }
        if entry.captureCompleteness == .partial {
            parts.append("Partial")
        }
        return parts.joined(separator: " · ")
    }
}

// MARK: - The week ahead

/// What the calendar says is coming, grouped by day, with the standing answers
/// each series already carries. The answers are read here and written in
/// Settings, which owns the series commands.
struct UpcomingView: View {
    let store: MeetingLiveStore
    var onOpenCalendarSettings: () -> Void = {}

    var body: some View {
        PageSection("Upcoming") {
            if store.upcomingLoading && store.upcoming == nil {
                Text("Reading your calendar…").bodyText(14, Theme.inkSecondary)
            } else if store.upcomingAccess != .authorized {
                VStack(alignment: .leading, spacing: 8) {
                    Text(access).bodyText(14, Theme.inkSecondary)
                    if store.upcomingAccess == .notDetermined
                        || store.upcomingAccess == .denied {
                        Button("Turn on “Use my calendar” in Settings") {
                            onOpenCalendarSettings()
                        }
                        .buttonStyle(.secondary)
                    }
                }
            } else if store.upcomingDays.isEmpty {
                Text("Nothing scheduled for the next 7 days.")
                    .bodyText(14, Theme.inkSecondary)
            } else {
                VStack(alignment: .leading, spacing: 20) {
                    ForEach(store.upcomingDays) { day in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(day.heading).sectionLabel()
                            ForEach(day.rows) { row in
                                UpcomingRowView(store: store, row: row)
                            }
                        }
                    }
                }
            }
        }
    }

    private var access: String {
        switch store.upcomingAccess {
        case .unavailable: "This system has no calendar Sona can read."
        default: "Sona cannot see your calendar."
        }
    }
}

/// One calendar row: the facts, the press, and what the series already
/// answered for itself.
struct UpcomingRowView: View {
    let store: MeetingLiveStore
    let row: UpcomingRow

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            MeetingStartPreviewCard(
                facts: MeetingStartFacts(row: row),
                armed: MeetingSourceKind.allCases,
                notesTemplate: row.series?.template ?? store.notesTemplate,
                templateFromSeries: row.series?.template != nil,
                onRecord: { store.start(row) })
            if let series = row.series {
                Card {
                    CardRow {
                        Text("Repeats").metaText()
                    } trailing: {
                        VStack(alignment: .trailing, spacing: 2) {
                            Text("Always record this series: \(series.alwaysRecord ? "On" : "Off")")
                                .metaText()
                            Text("Notes template: \(series.template?.label ?? "App default")")
                                .metaText()
                            Text(
                                "Include in the evening digest: "
                                    + (series.digestIncluded ? "On" : "Off")
                            )
                            .metaText()
                        }
                    }
                }
            }
        }
    }
}

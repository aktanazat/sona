import SwiftUI

/// The meetings section's parts: every recorded meeting, what Sona heard
/// across them, the trash, and the way into one. The shell lays them out on
/// the meetings page under what leads to a recording, and hands over to
/// `MeetingReviewView` when a meeting is open.

// MARK: - What the store has to say

/// The line where a write lands: "Deleted. It waits in the trash for a week."
/// Also carries the path a ledger was written to, with the way to open it.
struct MeetingsNoticeBand: View {
    let store: MeetingsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let notice = store.notice {
                band(notice, accent: false) {
                    Button("OK") { store.dismissNotice() }
                        .buttonStyle(QuietButton())
                }
            }
            if let error = store.error {
                band(error, accent: true) {
                    Button("Dismiss") { store.dismissError() }
                        .buttonStyle(QuietButton())
                }
            }
            if store.savedLedgerPath != nil {
                band("The ledger is written.", accent: false) {
                    HStack(spacing: 12) {
                        Button("Show in Finder") { store.openSavedLedger() }
                            .buttonStyle(SecondaryButton(compact: true))
                        Button("Dismiss") { store.dismissSavedLedger() }
                            .buttonStyle(QuietButton())
                    }
                }
            }
        }
        .padding(.bottom, store.notice == nil && store.error == nil && store.savedLedgerPath == nil ? 0 : 20)
    }

    private func band<Action: View>(
        _ text: String, accent: Bool, @ViewBuilder action: () -> Action
    ) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            Text(text).bodyText(14, accent ? Theme.live : Theme.ink)
            Spacer(minLength: 12)
            action()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(accent ? Theme.surface : Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusCard))
        .overlay(
            RoundedRectangle(cornerRadius: Theme.radiusCard)
                .strokeBorder(accent ? Theme.live.opacity(0.4) : Theme.border, lineWidth: 1)
        )
    }
}

/// The list failed to read. The message, and the one button that helps.
struct MeetingsRetryNote: View {
    let message: String
    let retry: () -> Void

    var body: some View {
        PageSection("Meetings") {
            Card {
                CardRow {
                    Text(message).bodyText(15, Theme.live)
                } trailing: {
                    Button("Try again", action: retry)
                        .buttonStyle(SecondaryButton(compact: true))
                }
            }
        }
        .padding(.bottom, 28)
    }
}

// MARK: - The trend

/// What Sona heard over a window: the totals, and one bar a day.
struct MeetingsTrendCard: View {
    let store: MeetingsStore

    var body: some View {
        PageSection("What Sona heard") {
            Card {
                ChoiceRow(
                    title: "Window",
                    detail: store.trend?.span,
                    choices: MeetingTrendRange.allCases,
                    label: { $0.label },
                    selection: range
                )
                if let totals = store.trend?.rangeTotal {
                    CardRow {
                        HStack(alignment: .top, spacing: 40) {
                            Stat(label: "Meetings", value: "\(totals.meetings)")
                            Stat(label: "Captured", value: totals.capturedSpoken)
                            Stat(label: "Lines said", value: "\(totals.transcriptSegments)")
                            Stat(label: "Action items", value: "\(totals.generatedActionItems)")
                        }
                    }
                    if !points.isEmpty {
                        CardRow {
                            MeetingsTrendBars(points: points)
                        }
                    }
                } else if store.trend != nil {
                    CardRow {
                        Text("Sona cannot reach its meeting storage, so there is no trend to show.")
                            .bodyText(14, Theme.inkSecondary)
                    }
                } else {
                    CardRow {
                        Text("Reading the trend…").bodyText(14, Theme.inkTertiary)
                    }
                }
            }
        }
        .padding(.bottom, 28)
    }

    private var points: [MeetingTrendPoint] { store.trend?.points ?? [] }

    private var range: Binding<MeetingTrendRange> {
        Binding(get: { store.trendRange }, set: { store.choose(trendRange: $0) })
    }
}

/// One bar a day, scaled to the loudest day in the window. A day with nothing
/// recorded keeps its place as a hairline.
struct MeetingsTrendBars: View {
    let points: [MeetingTrendPoint]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .bottom, spacing: 3) {
                ForEach(points) { point in
                    RoundedRectangle(cornerRadius: 2)
                        .fill(point.meetings > 0 ? Theme.accent : Theme.selection)
                        .frame(height: max(2, 56 * fraction(point)))
                        .help("\(point.localDate): \(label(point))")
                }
            }
            .frame(height: 56, alignment: .bottom)
            if let first = points.first, let last = points.last {
                HStack {
                    Text(first.localDate).metaText()
                    Spacer(minLength: 12)
                    Text(last.localDate).metaText()
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var peak: Int64 {
        max(1, points.map(\.verifiedCapturedDurationMs).max() ?? 1)
    }

    private func fraction(_ point: MeetingTrendPoint) -> Double {
        Double(point.verifiedCapturedDurationMs) / Double(peak)
    }

    private func label(_ point: MeetingTrendPoint) -> String {
        let meetings = point.meetings == 1 ? "1 meeting" : "\(point.meetings) meetings"
        return "\(meetings), \((TimeInterval(point.verifiedCapturedDurationMs) / 1000).spoken)"
    }
}

// MARK: - Search and filters

struct MeetingsFilterCard: View {
    let store: MeetingsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            SearchField(prompt: "Search titles", text: query)
            Card {
                ChoiceRow(
                    title: "State",
                    choices: MeetingStatusFilter.allCases,
                    label: { $0.label },
                    selection: status
                )
                ChoiceRow(
                    title: "When",
                    choices: MeetingTimeWindow.allCases,
                    label: { $0.label },
                    selection: window
                )
            }
        }
        .padding(.bottom, 28)
    }

    private var query: Binding<String> {
        Binding(get: { store.query }, set: { store.search($0) })
    }

    private var status: Binding<MeetingStatusFilter> {
        Binding(get: { store.status }, set: { store.choose(status: $0) })
    }

    private var window: Binding<MeetingTimeWindow> {
        Binding(get: { store.window }, set: { store.choose(window: $0) })
    }
}

// MARK: - The list itself

struct MeetingsFeed: View {
    let store: MeetingsStore
    @State private var pendingDelete: MeetingHistorySummary?

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            if store.loading, store.entries.isEmpty {
                MeetingsSkeleton()
            } else if store.entries.isEmpty {
                Card {
                    CardRow {
                        Text(store.emptyLine).bodyText(15, Theme.inkSecondary)
                    }
                }
            } else {
                ForEach(store.groups) { group in
                    PageSection(group.heading) {
                        Card {
                            ForEach(group.items) { entry in
                                MeetingsRow(
                                    entry: entry,
                                    open: { store.open(entry.sessionId) },
                                    export: { store.export(entry.sessionId, format: $0) },
                                    ledger: { store.exportLedger(entry.sessionId) },
                                    delete: { pendingDelete = entry }
                                )
                            }
                        }
                    }
                }
            }
        }
        .alert("Delete this meeting?", isPresented: confirming, presenting: pendingDelete) { entry in
            Button("Delete", role: .destructive) {
                store.delete(entry.sessionId)
                pendingDelete = nil
            }
            Button("Keep it", role: .cancel) { pendingDelete = nil }
        } message: { entry in
            Text("“\(entry.title)” goes to the trash for a week, then Sona removes it for good.")
        }
    }

    private var confirming: Binding<Bool> {
        Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } })
    }
}

/// One meeting: when it started, what it was, what Sona heard, and how long
/// it ran. The menu carries the three things a list row can do.
struct MeetingsRow: View {
    let entry: MeetingHistorySummary
    let open: () -> Void
    let export: (MeetingExportFormat) -> Void
    let ledger: () -> Void
    let delete: () -> Void

    var body: some View {
        CardRow(action: open) {
            HStack(alignment: .top, spacing: 16) {
                Text(entry.date.time)
                    .font(TypeScale.mono(12))
                    .foregroundStyle(Theme.inkTertiary)
                    .frame(width: 62, alignment: .leading)
                VStack(alignment: .leading, spacing: 4) {
                    Text(entry.title).headlineText()
                    if let headline = entry.headlineText {
                        Text(headline).metaText(Theme.inkSecondary).lineLimit(2)
                    }
                    HStack(spacing: 10) {
                        if let attention = entry.attention {
                            Text(attention.text)
                                .font(TypeScale.label(12))
                                .foregroundStyle(attention.urgent ? Theme.live : Theme.accent)
                        }
                        if entry.captureCompleteness == .partial {
                            Text("Partial recording").metaText()
                        }
                        if let sources = entry.sources, !sources.isEmpty {
                            Text(sources.map(\.label).joined(separator: " + ")).metaText()
                        }
                    }
                }
            }
        } trailing: {
            HStack(spacing: 14) {
                if let duration = entry.durationShort {
                    Text(duration).font(TypeScale.mono(12)).foregroundStyle(Theme.inkSecondary)
                }
                Menu {
                    Button("Export as Markdown") { export(.markdown) }
                    Button("Export as JSON") { export(.json) }
                    if entry.hasLedger {
                        Button("Save the ledger as HTML", action: ledger)
                    }
                    Divider()
                    Button("Delete", role: .destructive, action: delete)
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.inkSecondary)
                        .frame(width: 28, height: 24)
                        .contentShape(Rectangle())
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
            }
        }
    }
}

/// While the first page reads: the shape of the rows, no invented content.
struct MeetingsSkeleton: View {
    var body: some View {
        PageSection("Loading") {
            Card {
                ForEach(0..<4, id: \.self) { _ in
                    CardRow {
                        VStack(alignment: .leading, spacing: 8) {
                            RoundedRectangle(cornerRadius: 4)
                                .fill(Theme.selection)
                                .frame(width: 220, height: 14)
                            RoundedRectangle(cornerRadius: 4)
                                .fill(Theme.selection.opacity(0.6))
                                .frame(width: 320, height: 11)
                        }
                    }
                }
            }
        }
    }
}

/// Newer and older, and which page this is. Absent while one page holds
/// everything.
struct MeetingsPager: View {
    let store: MeetingsStore

    var body: some View {
        if store.page > 1 || store.hasMore {
            HStack(spacing: 12) {
                Button("Newer") { store.previousPage() }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(store.page == 1)
                Text("Page \(store.page)").metaText()
                Button("Older") { store.nextPage() }
                    .buttonStyle(SecondaryButton(compact: true))
                    .disabled(!store.hasMore)
            }
            .padding(.top, 28)
        }
    }
}

// MARK: - The trash

/// Deleted meetings wait a week. Each row says when it goes, and offers the
/// way back.
struct MeetingsTrashSheet: View {
    let store: MeetingsStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Deleted meetings").font(TypeScale.headline).foregroundStyle(Theme.ink)
                    Text("Sona keeps a deleted meeting for a week, then removes it for good.")
                        .metaText(Theme.inkSecondary)
                }
                Spacer(minLength: 20)
                Button("Done") { store.closeTrash() }
                    .buttonStyle(.secondary)
            }
            .padding(24)
            Hairline()
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    if store.trashLoading, store.trash.isEmpty {
                        row { Text("Reading the trash…").bodyText(14, Theme.inkTertiary) }
                    } else if store.trash.isEmpty {
                        row { Text("Nothing deleted.").bodyText(14, Theme.inkSecondary) }
                    } else {
                        ForEach(store.trash) { entry in
                            CardRow {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(entry.title).bodyText()
                                    Text(store.expiry(of: entry)).metaText()
                                }
                            } trailing: {
                                Button("Restore") { store.restore(entry) }
                                    .buttonStyle(SecondaryButton(compact: true))
                                    .disabled(store.restoring != nil)
                            }
                        }
                    }
                }
            }
        }
        .frame(width: 520, height: 420)
        .background(Theme.page)
    }

    private func row<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        HStack { content() }
            .padding(.horizontal, 20)
            .padding(.vertical, 16)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

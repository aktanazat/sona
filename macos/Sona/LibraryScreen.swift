import SwiftUI

/// The Library: everything Sona has written, in day groups, with the receipts
/// behind each run and the recording it came from.
///
/// The collapsed log shows text, a count and a time. One row opens at a time
/// and grows the player, the actions and the receipts; that is what buys the
/// rest of the log its quiet.
struct LibraryScreen: View {
    let store: LibraryStore
    /// The file-import dialog belongs to another page; the button hands over.
    var importAudio: () -> Void = {}
    /// A correction is a vocabulary rule, and Vocabulary owns that write. The
    /// dialog collects the spoken span and the written replacement and hands
    /// the pair over.
    var addCorrection: (String, String) -> Void = { _, _ in }

    @State private var correcting: HistoryRow?
    @State private var reprocessing: HistoryRow?

    var body: some View {
        @Bindable var store = store
        Page {
            LibraryHeader(
                store: store,
                query: $store.query,
                importAudio: importAudio)
            ErrorNote(store.error)
            LibraryActivity(store: store)
            LibraryFeed(
                store: store,
                correct: { correcting = $0 },
                processAgain: { reprocessing = $0 })
            LibraryRetentionSection(store: store)
        }
        .task { await store.start() }
        .sheet(item: $correcting) { row in
            CorrectionSheet(row: row, save: addCorrection)
        }
        .sheet(item: $reprocessing) { row in
            ReprocessSheet(store: store, row: row)
        }
    }
}

// MARK: - The head of the page

/// The page's one line: the name of it, the field that searches it, the verb
/// that adds to it, and a menu for the rest. Under it, the one fact the page
/// has room for: the totals, or the match count while a search is running,
/// because the page answers one question at a time.
private struct LibraryHeader: View {
    let store: LibraryStore
    @Binding var query: String
    let importAudio: () -> Void

    private var searching: Bool {
        !store.activeQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private var settled: Bool {
        store.phase != .loading && store.phase != .error
    }

    /// "No matches", "12 matches", "30+ matches" while a search is running.
    private var matchLine: String {
        guard searching, settled else { return "" }
        let count = store.rows.count
        if count == 0 { return "No matches" }
        return store.hasMore ? "\(count)+ matches" : "\(count) matches"
    }

    /// "5 recordings · 3m 39s · 587 words". Nothing recorded, nothing to
    /// total: the feed below says that in a sentence already.
    private var summaryLine: String? {
        guard let stats = store.stats else {
            return store.statsLoading ? "Counting your recordings…" : "Usage statistics are unavailable."
        }
        guard stats.entries > 0 else { return nil }
        let length = LibraryClock.short(Double(stats.totalDurationMs) / 1000)
        return [
            LibraryCount.recordings(Int(stats.entries)),
            length,
            LibraryCount.words(Int(stats.totalWords)),
        ].joined(separator: " · ")
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 8) {
                Text("Library").titleText()
                meta
            }
            Spacer(minLength: 16)
            SearchField(prompt: "Search transcripts", text: $query)
                .frame(width: 240)
            Button("Import", action: importAudio)
                .buttonStyle(.primary)
            LibraryMenu(store: store)
        }
        .padding(.bottom, 28)
    }

    @ViewBuilder private var meta: some View {
        if searching {
            Text(matchLine).metaText(Theme.inkSecondary)
        } else if store.statsFailed {
            HStack(spacing: 12) {
                Text("Usage statistics are unavailable.")
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.live)
                Button("Try again") {
                    Task { await store.refreshStats() }
                }
                .buttonStyle(.compact)
            }
        } else if let summaryLine {
            Text(summaryLine).metaText(Theme.inkSecondary)
        }
    }
}

/// The two things the page can do that are not worth a button: read the log in
/// raw text, and open the folder the recordings are in.
private struct LibraryMenu: View {
    let store: LibraryStore

    var body: some View {
        Menu {
            Toggle(
                "Show raw text",
                isOn: Binding(
                    get: { store.textView == .raw },
                    set: { store.textView = $0 ? .raw : .processed }))
            Button("Open recordings folder") { store.openRecordingsFolder() }
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 15, weight: .medium))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 28, height: 36)
                .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("More")
    }
}

// MARK: - Activity

/// What the log looks like over the last week, month or half year: one bar per
/// day, and the three totals of the window under it.
private struct LibraryActivity: View {
    let store: LibraryStore

    var body: some View {
        if let trend = store.trend, !trend.points.isEmpty {
            PageSection("Activity") {
                Card {
                    CardRow {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(totals(trend)).bodyText()
                            Text(streak(trend)).metaText()
                        }
                    } trailing: {
                        Picker(
                            "",
                            selection: Binding(
                                get: { store.trendRange },
                                set: { store.trendRange = $0 })
                        ) {
                            ForEach(TrendRange.allCases, id: \.self) { range in
                                Text(range.label).tag(range)
                            }
                        }
                        .labelsHidden()
                        .pickerStyle(.menu)
                        .fixedSize()
                    }
                    TrendBars(points: trend.points)
                        .padding(.horizontal, 20)
                        .padding(.vertical, 18)
                }
            }
        }
    }

    private func totals(_ trend: TrendProjection) -> String {
        [
            LibraryCount.recordings(trend.rangeTotal.recordings),
            LibraryClock.short(Double(trend.rangeTotal.durationMs) / 1000),
            LibraryCount.words(trend.rangeTotal.words),
        ].joined(separator: " · ")
    }

    private func streak(_ trend: TrendProjection) -> String {
        "\(LibraryCount.days(trend.activeDays)) active · \(trend.currentStreakDays)-day streak"
    }
}

/// One bar per day, scaled to the busiest day in the window. A day with
/// nothing on it keeps its place and draws the track, because a gap in the row
/// is the fact.
private struct TrendBars: View {
    let points: [TrendPoint]

    private var peak: Int { max(points.map(\.recordings).max() ?? 0, 1) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .bottom, spacing: points.count > 60 ? 1 : 3) {
                ForEach(points) { point in
                    let fraction = Double(point.recordings) / Double(peak)
                    RoundedRectangle(cornerRadius: 2)
                        .fill(point.recordings == 0 ? Theme.selection : Theme.accent)
                        .frame(height: max(2, fraction * 64))
                        .frame(maxWidth: .infinity)
                        .help("\(TrendAxis.day(point.localDate)): \(LibraryCount.recordings(point.recordings))")
                }
            }
            .frame(height: 64, alignment: .bottom)
            HStack {
                Text(TrendAxis.day(points.first?.localDate ?? "")).metaText()
                Spacer()
                Text(TrendAxis.day(points.last?.localDate ?? "")).metaText()
            }
        }
    }
}

/// The trend's dates arrive as local calendar days, "2026-09-12", and are
/// printed as the day they name without moving through a time zone.
private enum TrendAxis {
    private static let wire: DateFormatter = {
        let formatter = DateFormatter()
        formatter.calendar = Calendar(identifier: .gregorian)
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()

    static func day(_ localDate: String) -> String {
        guard let date = wire.date(from: localDate) else { return localDate }
        return date.short
    }
}

// MARK: - The log

/// The log in day groups, and the four things it can be instead of rows: still
/// loading, unreadable, empty, or empty because a search matched nothing.
private struct LibraryFeed: View {
    let store: LibraryStore
    let correct: (HistoryRow) -> Void
    let processAgain: (HistoryRow) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            switch store.phase {
            case .loading:
                HistoryPlaceholder()
            case .error:
                Card {
                    CardRow {
                        Text("Couldn't load history.")
                            .font(TypeScale.body(14))
                            .foregroundStyle(Theme.live)
                    } trailing: {
                        Button("Try again") { store.reload() }
                            .buttonStyle(.compact)
                    }
                }
            default:
                if store.rows.isEmpty {
                    Card {
                        CardRow {
                            Text(emptyLine).metaText(Theme.inkSecondary)
                        }
                    }
                } else {
                    ForEach(store.days) { day in
                        HistoryDaySection(
                            store: store,
                            day: day,
                            correct: correct,
                            processAgain: processAgain)
                    }
                    HistoryFooter(store: store)
                }
            }
        }
        .padding(.bottom, 32)
    }

    private var emptyLine: String {
        let query = store.activeQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        return query.isEmpty ? "Dictations you make appear here." : "No transcripts match “\(query)”."
    }
}

/// Five bars the width a row of text will be. No labels over them: half a log
/// with blanks in it is noisier than the bars.
private struct HistoryPlaceholder: View {
    var body: some View {
        Card {
            ForEach(0..<5, id: \.self) { _ in
                HStack(spacing: 16) {
                    RoundedRectangle(cornerRadius: 3).fill(Theme.selection).frame(height: 12)
                    RoundedRectangle(cornerRadius: 3).fill(Theme.selection).frame(width: 48, height: 10)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 16)
                .overlay(alignment: .bottom) { Hairline() }
            }
        }
    }
}

/// One day: a heading over one card of rows, with the day's empty recordings
/// behind a single line at the end of it.
private struct HistoryDaySection: View {
    let store: LibraryStore
    let day: HistoryDay
    let correct: (HistoryRow) -> Void
    let processAgain: (HistoryRow) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(day.heading).metaText(Theme.inkSecondary)
            Card {
                ForEach(day.spoken) { row in
                    HistoryRowView(
                        store: store,
                        row: row,
                        correct: correct,
                        processAgain: processAgain)
                }
                if !day.silent.isEmpty {
                    HistorySilentGroup(
                        store: store,
                        rows: day.silent,
                        correct: correct,
                        processAgain: processAgain)
                }
            }
        }
    }
}

/// A day's worth of recordings the run left no words on, as one line to open.
/// Each would otherwise be a full row carrying a count of zero and the same
/// sentence about why there is nothing to read.
private struct HistorySilentGroup: View {
    let store: LibraryStore
    let rows: [HistoryRow]
    let correct: (HistoryRow) -> Void
    let processAgain: (HistoryRow) -> Void

    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                open.toggle()
            } label: {
                HStack(spacing: 8) {
                    Image(systemName: open ? "chevron.down" : "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.inkTertiary)
                    Text(rows.count == 1 ? "1 empty recording" : "\(rows.count) empty recordings")
                        .metaText()
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 14)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .overlay(alignment: .bottom) { Hairline() }
            if open {
                ForEach(rows) { row in
                    HistoryRowView(
                        store: store,
                        row: row,
                        correct: correct,
                        processAgain: processAgain)
                }
            }
        }
    }
}

/// The end of the feed: what the next page is doing, and the trip wire that
/// asks for it when the reader gets here.
private struct HistoryFooter: View {
    let store: LibraryStore

    var body: some View {
        HStack(spacing: 12) {
            switch store.phase {
            case .paging:
                Text("Loading history…").metaText()
            case .pagingError:
                Text("Couldn't load history.")
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.live)
                Button("Try again") { store.loadMore() }
                    .buttonStyle(.compact)
            default:
                if store.hasMore {
                    Button("Load more") { store.loadMore() }
                        .buttonStyle(.compact)
                    Color.clear
                        .frame(width: 1, height: 1)
                        .onScrollVisibilityChange(threshold: 0.1) { visible in
                            if visible { store.loadMore() }
                        }
                }
            }
        }
        .frame(maxWidth: .infinity)
    }
}

// MARK: - One row

/// One dictation. Collapsed it is a line of text, a word count and a clock
/// time; the whole row is the disclosure. Open it grows everything the row can
/// do about that recording.
private struct HistoryRowView: View {
    let store: LibraryStore
    let row: HistoryRow
    let correct: (HistoryRow) -> Void
    let processAgain: (HistoryRow) -> Void

    @State private var hovering = false

    private var expanded: Bool { store.expanded == row.id }
    private var retrying: Bool { store.retrying.contains(row.id) }
    private var busy: Bool { retrying || store.deleting.contains(row.id) }
    private var receipt: HistoryRunReceipt? { store.latestReceipt(for: row.id) }
    private var noSpeech: Bool { receipt?.captureStatus == .noSpeechDetected }
    private var shown: String { row.text(store.textView) }

    /// A capture with no speech in it is not credited with a word count, and a
    /// row whose receipt has not arrived states none rather than guessing one
    /// from the text it happens to be showing.
    private var words: Int? {
        guard !noSpeech else { return nil }
        return receipt?.wordCount
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { store.toggleExpanded(row.id) } label: { line }
                .buttonStyle(.plain)
                .background(hovering && !expanded ? Theme.wash : .clear)
                .onHover { hovering = $0 }
            if expanded {
                Hairline().padding(.horizontal, 20)
                HistoryRowDetail(
                    store: store,
                    row: row,
                    retrying: retrying,
                    busy: busy,
                    shown: shown,
                    correct: correct,
                    processAgain: processAgain)
            }
        }
        .overlay(alignment: .bottom) { Hairline() }
        .task(id: row.id) { await store.loadReceipts(for: row.id) }
    }

    private var line: some View {
        HStack(alignment: .firstTextBaseline, spacing: 16) {
            Text(stated.text)
                .bodyText(14, stated.tone)
                .lineLimit(expanded ? nil : 2)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
            if row.matchKind == .semantic {
                Text("by meaning").metaText(Theme.inkSecondary)
            }
            if let words {
                Text(LibraryCount.words(words))
                    .font(TypeScale.mono(13))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(minWidth: 62, alignment: .trailing)
            }
            Text(row.item.date.time)
                .font(TypeScale.mono(13))
                .foregroundStyle(Theme.inkSecondary)
                .frame(minWidth: 44, alignment: .trailing)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    /// The one line the collapsed row states, in priority order: a retry in
    /// flight owns it whatever the last run concluded, a capture with no
    /// speech in it says so, text is the text, and no text names why there is
    /// none. Only the third is the row's own content, so only it reads at full
    /// contrast; the other three are the app talking about the row.
    private var stated: (text: String, tone: Color) {
        if retrying { return ("Transcribing…", Theme.inkSecondary) }
        if noSpeech { return ("No speech detected", Theme.inkSecondary) }
        if !shown.isEmpty { return (shown, Theme.ink) }
        return (HistoryEmptyLine.reason(receipt), Theme.inkSecondary)
    }
}

/// Why an empty transcript is empty. Only three run outcomes reach a complete
/// capture with no text, and the receipt tells them apart: the held cloud path
/// sets its own status, the failure path names no engine, and everything else
/// is a run whose words post-processing removed. Anything short of a complete
/// capture gets the neutral statement, which is true of all of them.
private enum HistoryEmptyLine {
    static func reason(_ receipt: HistoryRunReceipt?) -> String {
        if let receipt, receipt.captureStatus == .complete {
            if receipt.mode.cloudStatus == .heldCloudUnavailable {
                return "Sona held the cloud result: nothing trustworthy came back and no local model was available."
            }
            if receipt.mode.engineUsed == nil {
                return "Transcription failed, so nothing was recorded."
            }
        }
        return "No text was recorded for this entry."
    }
}

/// The open row: why the mode wrote nothing when it did, the recording, the
/// two things you open a row to do, and the receipts underneath.
private struct HistoryRowDetail: View {
    let store: LibraryStore
    let row: HistoryRow
    let retrying: Bool
    let busy: Bool
    let shown: String
    let correct: (HistoryRow) -> Void
    let processAgain: (HistoryRow) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if row.processedMissing(store.textView), !retrying {
                Text("Post-processing produced no text, so this is the raw transcript.")
                    .metaText()
            }
            if store.playable(row.id) {
                PlaybackBar(store: store, id: row.id)
            }
            controls
            ReceiptInspector(store: store, row: row)
        }
        .padding(20)
    }

    private var controls: some View {
        HStack(spacing: 8) {
            Button(store.copied == row.id ? "Copied" : "Copy") {
                store.copy(row.id, text: shown)
            }
            .buttonStyle(.compact)
            .disabled(shown.isEmpty || busy)

            Button("Transcribe again") { store.retryTranscription(row.id) }
                .buttonStyle(.compact)
                .disabled(busy)

            Menu {
                Button("Add correction") { correct(row) }
                    .disabled(shown.isEmpty || busy)
                Button(row.item.saved ? "Remove from saved" : "Save entry") {
                    store.toggleSaved(row.id)
                }
                .disabled(busy)
                Button("Process again") { processAgain(row) }
                    .disabled(busy)
                Divider()
                Button("Delete", role: .destructive) { store.delete(row.id) }
                    .disabled(busy)
            } label: {
                Image(systemName: "ellipsis")
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(Theme.inkSecondary)
                    .frame(width: 26, height: 30)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityLabel("More")
            Spacer(minLength: 0)
        }
    }
}

// MARK: - Playback

/// The stored recording: one control, one scrubber, one clock. The bytes are
/// read from the core on the first play and the row keeps them until another
/// row takes the player.
private struct PlaybackBar: View {
    let store: LibraryStore
    let id: Int64

    private var active: Bool { store.playingId == id }
    private var loading: Bool { store.loadingAudio == id }

    private var total: TimeInterval {
        active && store.duration > 0 ? store.duration : (store.statedLength(id) ?? 0)
    }

    private var fraction: Double {
        guard active, store.duration > 0 else { return 0 }
        return store.position / store.duration
    }

    var body: some View {
        HStack(spacing: 12) {
            Button { store.togglePlayback(for: id) } label: {
                Image(systemName: active && store.playing ? "pause.fill" : "play.fill")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(loading ? Theme.inkDisabled : Theme.ink)
                    .frame(width: 26, height: 26)
                    .background(Theme.inset, in: Circle())
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .disabled(loading)

            PlaybackScrubber(fraction: fraction) { store.seek($0) }

            Text("\(LibraryClock.short(active ? store.position : 0)) / \(LibraryClock.short(total))")
                .font(TypeScale.mono(13))
                .foregroundStyle(Theme.inkSecondary)
        }
        .frame(maxWidth: 420, alignment: .leading)
    }
}

/// The scrubber: the page's own meter, with the head draggable along it.
private struct PlaybackScrubber: View {
    let fraction: Double
    let seek: (Double) -> Void

    var body: some View {
        GeometryReader { proxy in
            Meter(fraction: min(max(fraction, 0), 1))
                .frame(height: 4)
                .frame(maxHeight: .infinity)
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 0)
                        .onChanged { value in
                            seek(value.location.x / max(proxy.size.width, 1))
                        })
        }
        .frame(height: 20)
    }
}

// MARK: - Receipts

/// The full receipt, as plain key and value text under the row's own hairline.
/// Three ways to have none, and they are not the same thing, so the panel says
/// which.
private struct ReceiptInspector: View {
    let store: LibraryStore
    let row: HistoryRow

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            // A reprocess and a retry both write a new row pointing at the one
            // they came from. Naming the id says which row.
            if let parentId = row.parentId {
                Text("from #\(parentId)")
                    .font(TypeScale.mono(13))
                    .foregroundStyle(Theme.inkTertiary)
            }
            switch store.receipts[row.id] {
            case .none, .loading:
                Text("Loading run receipts…").metaText()
            case .failed:
                Text("Run receipt data is unavailable for this entry.").metaText()
            case let .ready(list):
                if list.isEmpty {
                    Text("No run receipts were recorded for this entry.").metaText()
                } else {
                    ForEach(list) { receipt in
                        ReceiptCard(receipt: receipt)
                    }
                }
            }
        }
    }
}

/// One run, as the machine recorded it: the settings it ran under, what it
/// measured, which context sources took part, and where the words went.
private struct ReceiptCard: View {
    let receipt: HistoryRunReceipt

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(spacing: 0) {
                ForEach(Array(pairs.enumerated()), id: \.element.id) { index, pair in
                    HStack(alignment: .firstTextBaseline, spacing: 16) {
                        Text(pair.label).metaText()
                        Spacer(minLength: 12)
                        Text(pair.value)
                            .font(TypeScale.mono(13))
                            .foregroundStyle(pair.tone)
                            .multilineTextAlignment(.trailing)
                            .textSelection(.enabled)
                    }
                    .padding(.vertical, 6)
                    .overlay(alignment: .top) {
                        if index > 0 { Hairline() }
                    }
                }
            }
            ReceiptTable(
                title: "Context sources",
                columns: ("Source", "Status"),
                rows: receipt.context.sources.listed.map { ReceiptTableRow(id: $0.0, header: $0.0, value: $0.1) })
            ReceiptTable(
                title: "Delivery attempts",
                columns: ("Method", "Outcome"),
                empty: "No delivery attempt was recorded.",
                rows: receipt.deliveryAttempts.map {
                    ReceiptTableRow(
                        id: String($0.id),
                        header: $0.delivery.method.label,
                        value: $0.delivery.outcome.label)
                })
        }
        .padding(.top, 2)
    }

    /// The receipt's key and value pairs, in the order the inspector prints
    /// them. A measurement the run did not record is left out rather than
    /// printed as a zero.
    private var pairs: [ReceiptPair] {
        var list: [ReceiptPair] = [
            ReceiptPair(id: "mode", label: "Mode", value: receipt.mode.modeId),
            ReceiptPair(id: "revision", label: "Settings version", value: "\(receipt.mode.settingsRevision)"),
        ]
        if let engine = receipt.mode.engineRequested {
            list.append(ReceiptPair(id: "engine", label: "Engine", value: engine.label))
        }
        if let kind = receipt.sourceKind {
            list.append(ReceiptPair(id: "source", label: "Source", value: kind.label))
        }
        if let capture = receipt.captureStatus {
            list.append(
                ReceiptPair(id: "capture", label: "Capture", value: capture.label, tone: tone(capture)))
        }
        if let duration = receipt.durationMs {
            list.append(ReceiptPair(id: "duration", label: "Duration", value: LibraryClock.milliseconds(duration)))
        }
        if let count = receipt.wordCount {
            list.append(ReceiptPair(id: "words", label: "Words", value: "\(count)"))
        }
        // The amplitudes are printed to four places, as the core logs them:
        // fewer digits turn a dead input and a quiet room into one number.
        if let peak = receipt.mode.inputPeak {
            list.append(ReceiptPair(id: "peak", label: "Peak", value: String(format: "%.4f", peak)))
        }
        if let rms = receipt.mode.inputRms {
            list.append(ReceiptPair(id: "rms", label: "Average", value: String(format: "%.4f", rms)))
        }
        // Audio seconds per decode second, so the label says Decode: it
        // measures the decode span and excludes the model load.
        if let factor = receipt.mode.realtimeFactor {
            list.append(
                ReceiptPair(
                    id: "rtf", label: "Decode",
                    value: factor < 1 ? String(format: "%.2fx", factor) : String(format: "%.1fx", factor)))
        }
        list.append(ReceiptPair(id: "preset", label: "Preset", value: receipt.mode.promptPreset.label))
        list.append(ReceiptPair(id: "context", label: "Context level", value: receipt.mode.contextPolicy.label))
        list.append(
            ReceiptPair(
                id: "completed", label: "Completed",
                value: Date(timeIntervalSince1970: TimeInterval(receipt.completedAtMs) / 1000).time))
        if let provider = receipt.mode.providerId {
            let model = receipt.mode.modelId.map { " · \($0)" } ?? ""
            list.append(ReceiptPair(id: "provider", label: "AI route", value: provider + model))
        }
        return list
    }

    /// The state word carries the only colour, and only when the state is not
    /// the ordinary one. A no-speech capture is a real outcome of a real
    /// capture, so it steps back rather than reading as a failure.
    private func tone(_ status: ReceiptCaptureStatus) -> Color {
        switch status {
        case .complete: Theme.ink
        case .truncated: Theme.accent
        case .noSpeechDetected: Theme.inkSecondary
        }
    }
}

private struct ReceiptPair: Identifiable {
    let id: String
    let label: String
    let value: String
    var tone: Color = Theme.ink
}

private struct ReceiptTableRow: Identifiable {
    let id: String
    let header: String
    let value: String
}

/// The receipt's named-column tables: row header left, value right, one
/// hairline per pair. Both callers hand it the same shape.
private struct ReceiptTable: View {
    let title: String
    let columns: (String, String)
    var empty: String?
    let rows: [ReceiptTableRow]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).metaText(Theme.inkSecondary)
            if rows.isEmpty, let empty {
                Text(empty).metaText()
            } else {
                VStack(spacing: 0) {
                    HStack(alignment: .firstTextBaseline, spacing: 16) {
                        Text(columns.0).metaText(Theme.inkTertiary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text(columns.1).metaText(Theme.inkTertiary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.vertical, 6)
                    ForEach(rows) { row in
                        HStack(alignment: .firstTextBaseline, spacing: 16) {
                            Text(row.header)
                                .font(TypeScale.body(13))
                                .foregroundStyle(Theme.inkSecondary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text(row.value)
                                .font(TypeScale.mono(13))
                                .foregroundStyle(Theme.ink)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .padding(.vertical, 6)
                        .overlay(alignment: .top) { Hairline() }
                    }
                }
            }
        }
    }
}

// MARK: - What is kept

/// How much of this survives: how many entries the core keeps, how long their
/// recordings live after the words were written, and whether the log is
/// encrypted where it sits.
private struct LibraryRetentionSection: View {
    let store: LibraryStore

    @State private var limitText = ""
    @FocusState private var editingLimit: Bool

    var body: some View {
        PageSection("What is kept") {
            Card {
                CardRow {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Dictations to keep").bodyText()
                        Text("Set 0 to disable saved history.").metaText()
                    }
                } trailing: {
                    InputField(prompt: "5", text: $limitText)
                        .frame(width: 88)
                        .focused($editingLimit)
                        .disabled(store.savingRetention)
                        .onSubmit { commit() }
                }
                ChoiceRow(
                    title: "Delete recordings after",
                    choices: LibraryRetention.allCases,
                    label: { $0.label },
                    selection: Binding(
                        get: { store.retention },
                        set: { store.updateRetention($0) }))
                if let storage = store.storage {
                    CardRow {
                        Text(storage.line).metaText(Theme.inkSecondary)
                    }
                }
            }
        }
        .onAppear { limitText = "\(store.limit)" }
        .onChange(of: store.limit) { _, value in
            if !editingLimit { limitText = "\(value)" }
        }
        .onChange(of: editingLimit) { _, focused in
            if !focused { commit() }
        }
    }

    /// A field that does not hold a number is not a limit; the last accepted
    /// one comes back rather than a zero the reader never typed.
    private func commit() {
        guard let value = Int(limitText.trimmingCharacters(in: .whitespaces)), value >= 0 else {
            limitText = "\(store.limit)"
            return
        }
        store.updateLimit(value)
        limitText = "\(min(value, LibraryStore.limitCeiling))"
    }
}

// MARK: - Dialogs

/// Teach Sona a word it keeps getting wrong: the exact spoken span, and what
/// it should write instead. The pair goes to the vocabulary, which owns the
/// rule and the list it joins.
struct CorrectionSheet: View {
    let row: HistoryRow
    let save: (String, String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var spoken = ""
    @State private var written = ""

    private var ready: Bool {
        !spoken.trimmingCharacters(in: .whitespaces).isEmpty
            && !written.trimmingCharacters(in: .whitespaces).isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Add vocabulary correction").headlineText()
                Text("Enter the exact spoken span and the text Sona should write. Nothing is learned from typing or background activity.")
                    .metaText()
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("Spoken span").metaText(Theme.inkSecondary)
                InputField(prompt: "What Sona heard", text: $spoken)
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("Written replacement").metaText(Theme.inkSecondary)
                InputField(prompt: "What Sona should write", text: $written)
            }
            if ready {
                Text("\(spoken.trimmingCharacters(in: .whitespaces)) → \(written.trimmingCharacters(in: .whitespaces))")
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.ink)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Theme.inset, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
            }
            HStack(spacing: 10) {
                Spacer()
                Button("Cancel") { dismiss() }
                    .buttonStyle(.secondary)
                Button("Save correction") {
                    save(
                        spoken.trimmingCharacters(in: .whitespaces),
                        written.trimmingCharacters(in: .whitespaces))
                    dismiss()
                }
                .buttonStyle(.primary)
                .disabled(!ready)
            }
        }
        .padding(24)
        .frame(width: 440)
        .background(Theme.page)
    }
}

/// Run this recording through another mode. The original entry is kept and the
/// result is saved as a new one.
struct ReprocessSheet: View {
    let store: LibraryStore
    let row: HistoryRow

    @Environment(\.dismiss) private var dismiss
    @State private var modes: [ReprocessMode] = []
    @State private var selected: String?
    @State private var busy = false
    @State private var failure: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Process again").headlineText()
                Text("Run this recording through another mode. The original entry is kept and the result is saved as a new one.")
                    .metaText()
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("Mode").metaText(Theme.inkSecondary)
                Picker("", selection: $selected) {
                    Text("Choose a mode").tag(String?.none)
                    ForEach(modes) { mode in
                        Text(mode.name).tag(String?.some(mode.id))
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .disabled(busy || modes.isEmpty)
            }
            if let failure {
                Text(failure)
                    .font(TypeScale.body(14))
                    .foregroundStyle(Theme.live)
                    .textSelection(.enabled)
            }
            HStack(spacing: 10) {
                Spacer()
                Button("Cancel") { dismiss() }
                    .buttonStyle(.secondary)
                    .disabled(busy)
                Button("Process") { run() }
                    .buttonStyle(.primary)
                    .disabled(busy || selected == nil)
            }
        }
        .padding(24)
        .frame(width: 440)
        .background(Theme.page)
        .task {
            do {
                let list = try await store.reprocessModes()
                modes = list.modes
                if selected == nil { selected = list.activeModeId }
            } catch {
                failure = error.localizedDescription
            }
        }
    }

    private func run() {
        guard let selected, !busy else { return }
        busy = true
        failure = nil
        Task {
            do {
                try await store.reprocess(row.id, modeId: selected)
                dismiss()
            } catch {
                failure = error.localizedDescription
            }
            busy = false
        }
    }
}

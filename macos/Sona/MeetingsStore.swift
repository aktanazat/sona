import AppKit
import Foundation
import Observation

// MARK: - What the screens show

/// The three readings of one recorded meeting, in the order the review offers
/// them. Which one opens first depends on what the core has written: a ledger
/// wins, then notes, then the words themselves.
enum MeetingReviewTab: String, CaseIterable, Identifiable {
    case transcript
    case insights
    case ledger

    var id: String { rawValue }

    var title: String {
        switch self {
        case .transcript: "Transcript"
        case .insights: "Insights"
        case .ledger: "Ledger"
        }
    }
}

/// One day of the history list: the heading a reader scans for, and the
/// meetings recorded under it.
struct MeetingsDayGroup: Identifiable {
    let id: String
    let heading: String
    let items: [MeetingHistorySummary]
}

/// The one word a list row says about a meeting that needs a person: red when
/// the core says why it failed, bronze when the meeting was never finished.
struct MeetingAttention {
    let text: String
    let urgent: Bool
}

/// Consecutive segments from one voice, read as one paragraph.
struct TranscriptTurn: Identifiable {
    let id: TranscriptSegmentId
    let speakerId: SpeakerId
    let segments: [TranscriptEffectiveSegment]

    /// The clock reading in the row's gutter: where the turn starts.
    var time: String { segments[0].base.startOffsetNs.meetingOffsetClock }
}

/// A citation press: which segment to land on, and a count so pressing the
/// same citation twice moves the reader twice.
struct SegmentJump: Equatable {
    let segmentId: TranscriptSegmentId
    let nonce: Int
}

/// A run of gaps the core reported for one track, one epoch and one reason,
/// collapsed the way `aggregateSourceGaps` collapses them.
struct MeetingGapRun: Identifiable {
    let id: String
    let reason: MeetingSourceGapReason
    var count: Int
    var startOffsetNs: Int64?
    var endOffsetNs: Int64?
    var durationNs: Int64?
    var droppedFrames: Int64?

    /// "3:12 – 3:18", one reading when the gap is instantaneous, and words
    /// when the core could not place it at all.
    var range: String {
        guard let start = startOffsetNs, let end = endOffsetNs else { return "time unknown" }
        return start == end
            ? start.meetingOffsetClock
            : "\(start.meetingOffsetClock) – \(end.meetingOffsetClock)"
    }

    /// `formatGapDuration`: milliseconds under a second, one decimal under ten
    /// seconds, whole seconds under a minute, a clock above it.
    var missing: String? {
        guard let durationNs else { return nil }
        if durationNs < 1_000_000_000 {
            let milliseconds = Int((Double(durationNs) / 1_000_000).rounded())
            return "\(durationNs == 0 ? 0 : max(1, milliseconds))ms"
        }
        if durationNs < 60_000_000_000 {
            let seconds = Double(durationNs) / 1_000_000_000
            return seconds < 10 ? String(format: "%.1fs", seconds) : "\(Int(seconds.rounded()))s"
        }
        return durationNs.meetingOffsetClock
    }
}

/// Where the notes a person typed stand with the core.
enum MeetingNotesSaveState: Equatable {
    case idle
    case unsaved
    case saving
    case saved
    case conflict

    var label: String? {
        switch self {
        case .idle, .conflict: nil
        case .unsaved: "Not saved yet"
        case .saving: "Saving…"
        case .saved: "Notes saved"
        }
    }
}

/// The three things a person does to a loop or a commitment.
enum LoopChange {
    case resolve(dropped: Bool)
    case reopen
    case assign(personId: MeetingPersonId?)
}

/// One line of a traced summary: the words, and the moment they came from.
struct MeetingSummaryLine: Identifiable {
    let id: Int
    let text: String
    let segmentId: TranscriptSegmentId?
    let startOffsetNs: Int64?
}

// MARK: - Reading the wire a second way

extension MeetingProcessingStatus {
    /// The failure's reason word, for the one line the insights tab shows
    /// above its description.
    var failure: MeetingProcessingFailure? {
        if case let .failed(reason, _) = self { reason } else { nil }
    }

    var cause: MeetingEngineFailureCause? {
        if case let .failed(_, cause) = self { cause } else { nil }
    }

    /// Sending a person to Settings only helps for the two failures a setting
    /// can fix.
    var offersSettings: Bool {
        switch failure {
        case .localModelUnavailable, .remoteUnavailable: true
        default: false
        }
    }

    /// `FAILURE_CAUSES`: only these three are worth another run of the model.
    var offersRetry: Bool {
        switch cause {
        case .modelRefused, .replyNotStructured, .replyRejected: true
        default: false
        }
    }

    /// The sentence under the reason: why nothing was written.
    var explanation: String {
        switch self {
        case .pending, .running:
            "Sona is writing the notes for this meeting. This page fills in when it lands."
        case .succeeded:
            "Nothing was generated for this meeting."
        case .cancelled:
            "Processing was cancelled, so nothing was written."
        case let .failed(_, cause):
            cause.map { "Nothing was written because \($0.label)." }
                ?? "Sona could not write the notes for this meeting."
        }
    }
}

extension MeetingHistorySummary {
    /// `meetingCardStatus`, read as the one word the row needs.
    var attention: MeetingAttention? {
        switch processingStatus {
        case .failed, .cancelled:
            return MeetingAttention(text: processingStatus.label, urgent: true)
        default:
            break
        }
        return phase == .recoveryRequired
            ? MeetingAttention(text: "Needs attention", urgent: false)
            : nil
    }

    /// `formatDurationShort`: seconds, then minutes and seconds, then hours.
    var durationShort: String? {
        guard let recordedDurationMs else { return nil }
        let seconds = max(0, Int((Double(recordedDurationMs) / 1000).rounded()))
        if seconds < 60 { return "\(seconds)s" }
        let minutes = seconds / 60
        if minutes < 60 {
            let rest = seconds % 60
            return rest == 0 ? "\(minutes)m" : "\(minutes)m \(rest)s"
        }
        let restMinutes = minutes % 60
        return restMinutes == 0 ? "\(minutes / 60)h" : "\(minutes / 60)h \(restMinutes)m"
    }

    /// What a row shows under the title when the core has written something.
    var headlineText: String? { headline?.text }

    var hasLedger: Bool {
        if case .some(.ledger) = headline { true } else { false }
    }
}

extension MeetingTrendProjection {
    var range: MeetingTrendRange {
        switch self {
        case let .available(range, _, _, _, _, _): range
        case let .unavailable(range): range
        }
    }

    var allTime: MeetingTrendTotals? {
        if case let .available(_, _, _, allTime, _, _) = self { allTime } else { nil }
    }

    var rangeTotal: MeetingTrendTotals? {
        if case let .available(_, _, _, _, rangeTotal, _) = self { rangeTotal } else { nil }
    }

    var points: [MeetingTrendPoint] {
        if case let .available(_, _, _, _, _, points) = self { points } else { [] }
    }

    var span: String? {
        if case let .available(_, start, end, _, _, _) = self { "\(start) — \(end)" } else { nil }
    }
}

extension MeetingTrendTotals {
    /// The captured time the core stands behind, as a length in words.
    var capturedSpoken: String {
        (TimeInterval(verifiedCapturedDurationMs) / 1000).spoken
    }
}

extension ArtifactCitedText {
    /// `summaryLines`: the summary split into the lines the trace anchors,
    /// or nothing when no line has a moment behind it.
    func tracedLines(_ trace: [ArtifactSummaryLineTrace]?) -> [MeetingSummaryLine]? {
        guard let trace, !trace.isEmpty else { return nil }
        let anchors = Dictionary(trace.map { ($0.line, $0.anchor) }, uniquingKeysWith: { first, _ in first })
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
            .enumerated()
            .map { (index: $0.offset, text: $0.element.trimmingCharacters(in: .whitespaces)) }
            .filter { !$0.text.isEmpty }
            .map { line in
                MeetingSummaryLine(
                    id: line.index,
                    text: line.text,
                    segmentId: anchors[line.index]?.segmentId,
                    startOffsetNs: anchors[line.index]?.startOffsetNs
                )
            }
        return lines.allSatisfy { $0.segmentId == nil } ? nil : lines
    }
}

// MARK: - The store

/// Every recorded meeting and the one being read.
///
/// The list, its search and its trend live beside the open meeting because the
/// core's events arrive once for the whole section: a session ending changes
/// both the list and the page in front of a reader. Observers are registered
/// once, in `init`, for the life of the app.
@MainActor
@Observable
final class MeetingsStore {
    /// The page size `useMeetingsFeed` asks for.
    private static let pageSize = 25
    /// The debounce on the list's title search, on the transcript search, and
    /// on the notes a person is typing.
    private static let listSettle = Duration.milliseconds(200)
    private static let searchSettle = Duration.milliseconds(250)
    private static let notesSettle = Duration.milliseconds(1_200)
    private static let eventSettle = Duration.milliseconds(120)
    /// A day, for reading a trash entry's deadline.
    private static let dayMs: Int64 = 24 * 60 * 60 * 1_000

    // MARK: The list

    private(set) var entries: [MeetingHistorySummary] = []
    private(set) var hasMore = false
    private(set) var loading = true
    private(set) var query = ""
    private(set) var committedQuery = ""
    private(set) var status: MeetingStatusFilter = .any
    private(set) var window: MeetingTimeWindow = .any
    private(set) var listError: String?
    private(set) var trend: MeetingTrendProjection?
    private(set) var trendRange: MeetingTrendRange = .days30

    /// The cursor stack: one `created_at_utc_ms` per page walked back through.
    private var cursors: [Int64] = []
    var page: Int { cursors.count + 1 }

    // MARK: The deleted

    private(set) var trashOpen = false
    private(set) var trash: [MeetingTrashEntry] = []
    private(set) var trashLoading = false
    private(set) var restoring: MeetingDeletionJobId?

    // MARK: The open meeting

    private(set) var openSessionId: MeetingSessionId?
    private(set) var snapshot: MeetingReviewSnapshot?
    private(set) var reviewLoading = false
    private(set) var tab: MeetingReviewTab = .transcript
    private(set) var chosenTab: MeetingReviewTab?
    private(set) var receipt: MeetingOperationReceipt?
    private(set) var pending: String?
    private(set) var analytics: MeetingAnalyticsSnapshot?
    private(set) var loops: [MeetingLoopRow]?
    private(set) var loopsBusy = false
    private(set) var people: [MeetingPersonContextRow] = []
    private(set) var transcriptQuery = ""
    private(set) var searchHits: [MeetingSearchHit]?
    private(set) var jump: SegmentJump?
    private(set) var newNote = ""

    // MARK: What a person typed

    private(set) var userNotes: MeetingUserNotes?
    private(set) var notesBody = ""
    private(set) var notesState: MeetingNotesSaveState = .idle
    private(set) var enhancing = false
    private(set) var catchUp: MeetingCatchUp?
    private(set) var catchingUp = false

    // MARK: Sending it on

    private(set) var followUp: MeetingFollowUpDraft?
    private(set) var followUpOpen = false
    private(set) var drafting = false
    private(set) var savedLedgerPath: String?

    // MARK: What the store has to say

    /// The last thing the core refused, in the words React shows.
    private(set) var error: String?
    /// The last thing that worked, where React raises a toast.
    private(set) var notice: String?

    private let core: Core
    @ObservationIgnored private var listTask: Task<Void, Never>?
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var transcriptTask: Task<Void, Never>?
    @ObservationIgnored private var notesTask: Task<Void, Never>?
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    /// The revision each side read last, so a refresh that changed nothing
    /// does not re-ask for analytics or loops. Artifacts can be rewritten
    /// without the session's revision moving, so the generation counts too.
    @ObservationIgnored private var analyticsKey: String?
    @ObservationIgnored private var loopsKey: String?
    /// The note revision the core acknowledged: what the next save expects.
    @ObservationIgnored private var savedNoteRevision = 0
    @ObservationIgnored private var jumpNonce = 0

    init(core: Core) {
        self.core = core

        // A meeting that ends, or one whose phase moves, changes the list and
        // the page in front of a reader.
        core.observe(CoreEvent.meetingSessionChanged) { [weak self] line in
            self?.sessionChanged(line)
        }
        for name in [
            CoreEvent.meetingTranscriptChanged,
            CoreEvent.meetingNoteChanged,
            CoreEvent.meetingArtifactChanged,
            CoreEvent.meetingRemoteJobChanged,
        ] {
            core.observe(name) { [weak self] line in
                self?.sessionChanged(line)
            }
        }
        core.observe(CoreEvent.meetingRemoved) { [weak self] line in
            guard let self else { return }
            let payload: MeetingEventPayload? = try? Core.payload(line)
            if let removed = payload?.sessionId, removed == self.openSessionId {
                self.closeReview()
            }
            self.reloadListSoon()
        }
        core.observe(CoreEvent.meetingNavigationRequested) { [weak self] line in
            guard let self, let payload: MeetingNavigationPayload = try? Core.payload(line) else { return }
            switch payload.destination {
            case .list:
                self.closeReview()
            case .session, .preflight:
                if let sessionId = payload.sessionId {
                    self.open(sessionId)
                }
            }
        }
    }

    /// The first load: a page of meetings and the trend over them.
    func start() async {
        await loadPage()
        await loadTrend()
    }

    // MARK: - Events

    private func sessionChanged(_ line: Data) {
        let payload: MeetingEventPayload? = try? Core.payload(line)
        reloadListSoon()
        guard let sessionId = payload?.sessionId, sessionId == openSessionId else { return }
        eventTask?.cancel()
        eventTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingsStore.eventSettle)
            guard !Task.isCancelled else { return }
            await self?.refreshSnapshot()
        }
    }

    /// Coalesces a burst of events into one list read.
    private func reloadListSoon() {
        listTask?.cancel()
        listTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingsStore.eventSettle)
            guard !Task.isCancelled else { return }
            await self?.loadPage()
        }
    }

    // MARK: - The list

    func retry() {
        Task {
            await loadPage()
            await loadTrend()
        }
    }

    /// The search box. React commits the trimmed query 200ms after the last
    /// keystroke and resets to the first page.
    func search(_ text: String) {
        query = text
        searchTask?.cancel()
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingsStore.listSettle)
            guard !Task.isCancelled, let self else { return }
            let next = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard next != self.committedQuery else { return }
            self.committedQuery = next
            self.cursors = []
            await self.loadPage()
        }
    }

    func choose(status: MeetingStatusFilter) {
        guard status != self.status else { return }
        self.status = status
        cursors = []
        Task { await loadPage() }
    }

    func choose(window: MeetingTimeWindow) {
        guard window != self.window else { return }
        self.window = window
        cursors = []
        Task { await loadPage() }
    }

    func choose(trendRange range: MeetingTrendRange) {
        guard range != trendRange else { return }
        trendRange = range
        Task { await loadTrend() }
    }

    /// Older: the next page starts at the oldest row on this one.
    func nextPage() {
        guard hasMore, let oldest = entries.last else { return }
        cursors.append(oldest.createdAtUtcMs)
        Task { await loadPage() }
    }

    func previousPage() {
        guard !cursors.isEmpty else { return }
        cursors.removeLast()
        Task { await loadPage() }
    }

    /// The list, grouped by the day each meeting was recorded.
    var groups: [MeetingsDayGroup] {
        let calendar = Calendar.current
        var order: [Date] = []
        var days: [Date: [MeetingHistorySummary]] = [:]
        for entry in entries {
            let day = calendar.startOfDay(for: entry.date)
            if days[day] == nil {
                order.append(day)
                days[day] = []
            }
            days[day]?.append(entry)
        }
        return order.map { day in
            MeetingsDayGroup(
                id: "\(day.timeIntervalSince1970)",
                heading: day.relativeDay,
                items: days[day] ?? []
            )
        }
    }

    /// The line where the list would be: nothing recorded, or nothing matching.
    var emptyLine: String {
        committedQuery.isEmpty
            ? "No meetings yet. Record one and it lands here."
            : "No meetings match “\(committedQuery)”."
    }

    private func loadPage() async {
        loading = true
        defer { loading = false }
        do {
            let page: MeetingsPage = try await core.request(
                "meeting_list",
                MeetingRequest.list(
                    cursorUtcMs: cursors.last,
                    limit: MeetingsStore.pageSize,
                    titleQuery: committedQuery,
                    status: status,
                    window: window
                )
            )
            entries = page.entries
            hasMore = page.hasMore
            listError = nil
        } catch {
            listError = reason(error)
        }
    }

    private func loadTrend() async {
        do {
            trend = try await core.request("meeting_trend", MeetingRequest.trend(trendRange))
        } catch {
            trend = nil
        }
    }

    // MARK: - Deleting, and undoing it

    func openTrash() {
        trashOpen = true
        Task { await loadTrash() }
    }

    func closeTrash() {
        trashOpen = false
    }

    private func loadTrash() async {
        trashLoading = true
        defer { trashLoading = false }
        do {
            trash = try await core.request("meeting_trash_list")
        } catch {
            trash = []
            self.error = reason(error)
        }
    }

    /// "Deleted 12 Mar · gone in 5 days", the deadline read as whole days.
    func expiry(of entry: MeetingTrashEntry) -> String {
        let remaining = Double(entry.expiresAtUtcMs - Int64(Date().timeIntervalSince1970 * 1000))
        let days = Int((remaining / Double(MeetingsStore.dayMs)).rounded(.up))
        let deleted = "Deleted \(entry.deletedAt.short)"
        if days <= 0 { return "\(deleted) · gone today" }
        return days == 1 ? "\(deleted) · gone tomorrow" : "\(deleted) · gone in \(days) days"
    }

    func restore(_ entry: MeetingTrashEntry) {
        Task {
            restoring = entry.jobId
            defer { restoring = nil }
            do {
                let restored: MeetingSessionSnapshot = try await core.request(
                    "meeting_trash_restore", MeetingRequest.trashRestore(entry.jobId))
                notice = "“\(restored.title)” is back."
                await loadTrash()
                await loadPage()
            } catch {
                self.error = reason(error)
            }
        }
    }

    /// Delete a meeting the list is showing. The mutation needs the revision
    /// the core is on, so the snapshot is read first, exactly as the web app's
    /// row actions read it.
    func delete(_ sessionId: MeetingSessionId) {
        Task {
            guard let snapshot = await readSnapshot(sessionId) else { return }
            await act("Deleting") {
                let result: MeetingRemovalResult = try await self.core.request(
                    "meeting_delete",
                    MeetingRequest.wrap(MeetingRequest.mutation(sessionId, revision: snapshot.session.revision))
                )
                self.receive(result.receipt)
                if result.removed {
                    if sessionId == self.openSessionId { self.closeReview() }
                    self.notice = "Deleted. It waits in the trash for a week."
                }
                await self.loadPage()
            }
        }
    }

    // MARK: - Exporting

    /// Markdown or JSON. The core opens the save panel itself and answers with
    /// a receipt, so a cancelled panel is a typed error and not a failure.
    func export(_ sessionId: MeetingSessionId, format: MeetingExportFormat) {
        Task {
            guard let snapshot = await readSnapshot(sessionId) else { return }
            await act("Exporting") {
                let result: MeetingExportResult = try await self.core.request(
                    "meeting_export",
                    MeetingRequest.export(sessionId, revision: snapshot.session.revision, format: format)
                )
                self.receive(result.receipt)
                self.notice = "Exported as \(result.exportReceipt.format.label)."
                if sessionId == self.openSessionId { await self.refreshSnapshot() }
            }
        }
    }

    /// The ledger as one self-contained HTML file. The core writes it and
    /// answers with the path; `openSavedLedger` hands that path to the Finder.
    func exportLedger(_ sessionId: MeetingSessionId) {
        Task {
            await act("Writing the ledger") {
                let path: String = try await self.core.request(
                    "produce_ledger_html", MeetingRequest.session(sessionId))
                self.savedLedgerPath = path
                self.notice = "Ledger written to \(path)"
            }
        }
    }

    func openSavedLedger() {
        guard let savedLedgerPath else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: savedLedgerPath)])
        self.savedLedgerPath = nil
    }

    func dismissSavedLedger() {
        savedLedgerPath = nil
    }

    // MARK: - Opening one meeting

    func open(_ sessionId: MeetingSessionId) {
        transcriptTask?.cancel()
        openSessionId = sessionId
        snapshot = nil
        analytics = nil
        analyticsKey = nil
        loops = nil
        loopsKey = nil
        people = []
        receipt = nil
        chosenTab = nil
        tab = .transcript
        transcriptQuery = ""
        searchHits = nil
        jump = nil
        newNote = ""
        userNotes = nil
        notesBody = ""
        notesState = .idle
        catchUp = nil
        followUp = nil
        followUpOpen = false
        Task {
            reviewLoading = true
            defer { reviewLoading = false }
            await refreshSnapshot()
            await loadUserNotes()
            await loadPeople()
        }
    }

    func closeReview() {
        notesTask?.cancel()
        transcriptTask?.cancel()
        eventTask?.cancel()
        openSessionId = nil
        snapshot = nil
        followUpOpen = false
    }

    func choose(tab: MeetingReviewTab) {
        chosenTab = tab
        self.tab = tab
    }

    /// `meeting_get`. A meeting that is no longer there closes the page.
    func refreshSnapshot() async {
        guard let sessionId = openSessionId else { return }
        do {
            let next: MeetingReviewSnapshot = try await core.request(
                "meeting_get", MeetingRequest.session(sessionId))
            snapshot = next
            settleTab(next)
            await loadAnalytics(next)
            await loadLoops(next)
        } catch let failure as CoreError {
            if failure.remote(as: MeetingCommandError.self) == .notFound {
                closeReview()
                notice = "That meeting is no longer here."
                await loadPage()
                return
            }
            error = reason(failure)
        } catch {
            self.error = reason(error)
        }
    }

    /// `nextReviewTab`: a ledger opens on the ledger, written notes open on
    /// insights, and anything else opens on the words. A person's own choice
    /// stands.
    private func settleTab(_ snapshot: MeetingReviewSnapshot) {
        if let chosenTab {
            tab = chosenTab
            return
        }
        if snapshot.currentLedger != nil {
            tab = .ledger
        } else if !snapshot.artifacts.isEmpty || !snapshot.notes.isEmpty {
            tab = .insights
        } else {
            tab = .transcript
        }
    }

    private func readSnapshot(_ sessionId: MeetingSessionId) async -> MeetingReviewSnapshot? {
        if let snapshot, snapshot.session.sessionId == sessionId { return snapshot }
        do {
            return try await core.request("meeting_get", MeetingRequest.session(sessionId))
        } catch {
            self.error = reason(error)
            return nil
        }
    }

    // MARK: - The transcript

    var speakerNames: [SpeakerId: String] { snapshot?.speakerNames ?? [:] }

    func speakerName(_ speakerId: SpeakerId?) -> String {
        guard let speakerId, let name = speakerNames[speakerId] else { return "Unknown speaker" }
        return name
    }

    /// The words, filtered by whatever is in the search box: a hit from the
    /// core counts, and so does a plain match on the text a person can see.
    var turns: [TranscriptTurn] {
        guard let snapshot else { return [] }
        let needle = transcriptQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let hits = Set((searchHits ?? []).filter { $0.kind == .transcript }.map(\.entityId))
        var turns: [TranscriptTurn] = []
        var openSpeaker: SpeakerId?
        var openSegments: [TranscriptEffectiveSegment] = []
        func close() {
            if let openSpeaker, let first = openSegments.first {
                turns.append(TranscriptTurn(id: first.base.segmentId, speakerId: openSpeaker, segments: openSegments))
            }
            openSpeaker = nil
            openSegments = []
        }
        for segment in snapshot.transcript {
            let keep = needle.isEmpty
                || hits.contains(segment.base.segmentId)
                || segment.text.lowercased().contains(needle)
            guard keep else {
                close()
                continue
            }
            if openSpeaker == segment.assignedSpeakerId {
                openSegments.append(segment)
                continue
            }
            close()
            openSpeaker = segment.assignedSpeakerId
            openSegments = [segment]
        }
        close()
        return turns
    }

    /// Hits that are not in the transcript: a note, or the title.
    var elsewhereHits: [MeetingSearchHit] {
        (searchHits ?? []).filter { $0.kind != .transcript }
    }

    /// `aggregateSourceGaps`: a run of one reason on one track reads as one row.
    var gapRuns: [MeetingGapRun] {
        var runs: [MeetingGapRun] = []
        var keys: [(track: MeetingSourceTrackId, epoch: Int)] = []
        for gap in snapshot?.gaps ?? [] {
            let measured: Int64? = {
                guard let start = gap.startOffsetNs, let end = gap.endOffsetNs else { return nil }
                return max(0, end - start)
            }()
            let sameRun = keys.last?.track == gap.trackId
                && keys.last?.epoch == gap.epoch
                && runs.last?.reason == gap.reason
            guard sameRun, var last = runs.last else {
                runs.append(MeetingGapRun(
                    id: "\(gap.trackId):\(gap.epoch):\(gap.reason.rawValue):\(runs.count)",
                    reason: gap.reason,
                    count: 1,
                    startOffsetNs: gap.startOffsetNs,
                    endOffsetNs: gap.endOffsetNs,
                    durationNs: measured,
                    droppedFrames: gap.droppedFrames
                ))
                keys.append((gap.trackId, gap.epoch))
                continue
            }
            last.count += 1
            if last.endOffsetNs != nil, let end = gap.endOffsetNs {
                last.endOffsetNs = end
            } else {
                last.startOffsetNs = nil
                last.endOffsetNs = nil
            }
            last.durationNs = last.durationNs == nil || measured == nil ? nil : last.durationNs! + measured!
            last.droppedFrames = last.droppedFrames == nil || gap.droppedFrames == nil
                ? nil
                : last.droppedFrames! + gap.droppedFrames!
            runs[runs.count - 1] = last
        }
        return runs
    }

    /// The transcript search, 250ms after the last keystroke, scoped to this
    /// meeting.
    func searchTranscript(_ text: String) {
        transcriptQuery = text
        searchHits = nil
        transcriptTask?.cancel()
        let needle = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty, let sessionId = openSessionId else { return }
        transcriptTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingsStore.searchSettle)
            guard !Task.isCancelled, let self else { return }
            do {
                let result: MeetingSearchResult = try await self.core.request(
                    "meeting_search",
                    MeetingRequest.search(query: needle, sessionIds: [sessionId], limit: 50)
                )
                guard !Task.isCancelled else { return }
                self.searchHits = result.entries
            } catch {
                self.error = self.reason(error)
            }
        }
    }

    /// A citation press: the transcript tab, the search cleared, and the
    /// segment scrolled to.
    func jumpTo(_ segmentId: TranscriptSegmentId) {
        chosenTab = .transcript
        tab = .transcript
        transcriptQuery = ""
        searchHits = nil
        jumpNonce += 1
        jump = SegmentJump(segmentId: segmentId, nonce: jumpNonce)
    }

    func clearJump() {
        jump = nil
    }

    // MARK: - Editing what the core wrote

    var editable: Bool { snapshot?.session.allows(.edit) ?? false }
    var busy: Bool { pending != nil }
    var canRegenerate: Bool { snapshot?.session.allows(.regenerate) ?? false }
    var canExport: Bool { (snapshot?.canExport ?? false) && (snapshot?.session.allows(.export) ?? false) }
    var canDelete: Bool { snapshot?.session.allows(.delete) ?? false }
    var canCancelRemote: Bool {
        (snapshot?.remoteCancellationPending ?? false) && (snapshot?.session.allows(.cancelRemote) ?? false)
    }

    func setTitle(_ title: String) {
        mutate("Renaming", "meeting_title_set") { session in
            MeetingRequest.titleSet(session.sessionId, revision: session.revision, title: title)
        }
    }

    func renameSpeaker(_ speakerId: SpeakerId, to displayName: String) {
        mutate("Renaming the speaker", "meeting_speaker_rename") { session in
            MeetingRequest.speakerRename(
                session.sessionId, revision: session.revision,
                speakerId: speakerId, displayName: displayName)
        }
    }

    func mergeSpeaker(_ source: SpeakerId, into target: SpeakerId) {
        mutate("Merging the speakers", "meeting_speaker_merge") { session in
            MeetingRequest.speakerMerge(
                session.sessionId, revision: session.revision, source: source, target: target)
        }
    }

    func editSegment(_ segmentId: TranscriptSegmentId, text: String, removed: Bool = false) {
        mutate(removed ? "Removing the line" : "Correcting the line", "meeting_segment_edit") { session in
            MeetingRequest.segmentEdit(
                session.sessionId, revision: session.revision, segmentId: segmentId,
                replacementText: text, removed: removed)
        }
    }

    func setNewNote(_ text: String) {
        newNote = text
    }

    func createNote() {
        let body = newNote.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty else { return }
        newNote = ""
        mutate("Adding the note", "meeting_note_create") { session in
            MeetingRequest.noteCreate(
                session.sessionId, revision: session.revision,
                startOffsetNs: nil, body: body)
        }
    }

    func updateNote(_ note: MeetingManualNote, body: String) {
        mutate("Saving the note", "meeting_note_update") { session in
            MeetingRequest.noteUpdate(session.sessionId, revision: session.revision, note: note, body: body)
        }
    }

    func deleteNote(_ note: MeetingManualNote) {
        mutate("Deleting the note", "meeting_note_delete") { session in
            MeetingRequest.noteDelete(session.sessionId, revision: session.revision, note: note)
        }
    }

    func regenerate() {
        mutate("Rewriting the notes", "meeting_artifacts_regenerate") { session in
            MeetingRequest.wrap(MeetingRequest.mutation(session.sessionId, revision: session.revision))
        }
    }

    func forgetQuestion(_ questionId: MeetingQuestionId) {
        mutate("Forgetting the answer", "meeting_question_forget") { session in
            MeetingRequest.questionForget(
                session.sessionId, revision: session.revision, questionId: questionId)
        }
    }

    func cancelRemote() {
        mutate(
            "Cancelling the remote run", "meeting_remote_cancel",
            then: { self.notice = "Sona asked the remote engine to stop." }
        ) { session in
            MeetingRequest.wrap(MeetingRequest.mutation(session.sessionId, revision: session.revision))
        }
    }

    func exportOpenMeeting(_ format: MeetingExportFormat) {
        guard let sessionId = openSessionId else { return }
        export(sessionId, format: format)
    }

    func exportOpenLedger() {
        guard let sessionId = openSessionId else { return }
        exportLedger(sessionId)
    }

    func deleteOpenMeeting() {
        guard let sessionId = openSessionId else { return }
        delete(sessionId)
    }

    /// One mutation: build the params from the revision the store is on, send
    /// it, keep the receipt, then read the meeting back.
    private func mutate(
        _ action: String,
        _ method: String,
        then finish: (() -> Void)? = nil,
        params: @escaping (MeetingSessionSnapshot) -> [String: JSONValue]
    ) {
        guard let session = snapshot?.session else { return }
        Task {
            await act(action) {
                let result: MeetingMutationResult = try await self.core.request(method, params(session))
                self.receive(result.receipt)
                finish?()
                await self.refreshSnapshot()
                await self.loadPage()
            }
        }
    }

    /// A receipt is the core's answer about one write: a refusal is what the
    /// reader needs to see, and a committed write leaves the revision line.
    private func receive(_ receipt: MeetingOperationReceipt) {
        self.receipt = receipt
        if let refusal = receipt.refusal {
            error = refusal
        }
    }

    /// The line under the title after a write: "Saved as revision 12", plus
    /// whatever the core said about it.
    var receiptLine: String? {
        guard let receipt, receipt.sessionId == openSessionId else { return nil }
        let head = receipt.newRevision.map { "Saved as revision \($0)" } ?? "Saved"
        let reasons = receipt.reasonCodes.map(\.label).joined(separator: " · ")
        return reasons.isEmpty ? head : "\(head) — \(reasons)"
    }

    // MARK: - Artifacts and what is done

    /// The action items a person has ticked, as `artifactId:index` keys.
    var doneActionItems: Set<String> {
        Set((analytics?.actionItems ?? []).filter(\.done).map(\.key))
    }

    func isDone(_ artifactId: MeetingArtifactId, _ index: Int) -> Bool {
        doneActionItems.contains("\(artifactId):\(index)")
    }

    func toggleActionItem(_ artifactId: MeetingArtifactId, _ index: Int, done: Bool) {
        guard let sessionId = openSessionId else { return }
        Task {
            do {
                let states: [MeetingActionItemState] = try await core.request(
                    "set_action_item_done",
                    MeetingRequest.actionItemDone(
                        sessionId, artifactId: artifactId, actionIndex: index, done: done)
                )
                guard let current = analytics else { return }
                analytics = MeetingAnalyticsSnapshot(
                    sessionId: current.sessionId,
                    inputRevision: current.inputRevision,
                    computedAtUtcMs: current.computedAtUtcMs,
                    analytics: current.analytics,
                    actionItems: states,
                    notes: current.notes
                )
            } catch {
                self.error = reason(error)
            }
        }
    }

    /// What the analytics and the loops were read against: the session's
    /// revision, and which generation of the notes is current.
    private static func key(_ snapshot: MeetingReviewSnapshot) -> String {
        "\(snapshot.session.revision):\(snapshot.currentArtifact?.generationKey ?? "none")"
    }

    private func loadAnalytics(_ snapshot: MeetingReviewSnapshot) async {
        guard analyticsKey != MeetingsStore.key(snapshot) else { return }
        analyticsKey = MeetingsStore.key(snapshot)
        do {
            analytics = try await core.request(
                "get_meeting_analytics", MeetingRequest.session(snapshot.session.sessionId))
        } catch {
            analytics = nil
        }
    }

    /// The talk strip only reads when somebody said something.
    var talk: MeetingTalkMetrics? {
        guard let talk = analytics?.analytics.talk, talk.segmentCount > 0 else { return nil }
        return talk
    }

    var trackers: [MeetingTrackerResult] { analytics?.analytics.trackers ?? [] }

    /// "Ada 62% · Bo 31%": the two loudest, the way the strip's own band
    /// summarises itself.
    var talkLeaders: String {
        guard let talk else { return "" }
        return talk.speakers
            .sorted { $0.sharePermille > $1.sharePermille }
            .prefix(2)
            .map { "\(speakerName($0.speakerId)) \($0.sharePermille.meetingTalkShare)" }
            .joined(separator: " · ")
    }

    /// `formatPatience`: milliseconds under a second, one decimal above it.
    var patience: String {
        guard let gap = talk?.medianSwitchGapMs else { return "—" }
        return gap < 1_000 ? "\(gap)ms" : String(format: "%.1fs", Double(gap) / 1_000)
    }

    // MARK: - Loops and commitments

    private func loadLoops(_ snapshot: MeetingReviewSnapshot) async {
        guard loopsKey != MeetingsStore.key(snapshot) else { return }
        loopsKey = MeetingsStore.key(snapshot)
        await reloadLoops()
    }

    private func reloadLoops() async {
        guard let sessionId = openSessionId else { return }
        do {
            let result: MeetingLoopsResult = try await core.request(
                "meeting_loops", MeetingRequest.session(sessionId))
            loops = result.rows
        } catch {
            loops = []
        }
    }

    func loopRows(_ kind: MeetingLoopKind) -> [MeetingLoopRow] {
        (loops ?? []).filter { $0.kind == kind }
    }

    func change(_ row: MeetingLoopRow, _ change: LoopChange) {
        Task {
            loopsBusy = true
            defer { loopsBusy = false }
            do {
                let result: MeetingLoopMutationResult
                switch change {
                case let .resolve(dropped):
                    result = try await core.request(
                        "meeting_loop_resolve",
                        MeetingRequest.loopResolve(
                            row.loopId, revision: row.revision, resolution: dropped ? .dropped : .done)
                    )
                case .reopen:
                    result = try await core.request(
                        "meeting_loop_reopen",
                        MeetingRequest.loopReopen(row.loopId, revision: row.revision)
                    )
                case let .assign(personId):
                    result = try await core.request(
                        "meeting_loop_assign",
                        MeetingRequest.loopAssign(
                            row.loopId, revision: row.revision, ownerPersonId: personId)
                    )
                }
                loops = result.loops.rows
                if result.receipt.reasonCodes.contains(.staleRevision) {
                    notice = "Somebody else had already moved that one. This is the current state."
                }
            } catch {
                self.error = reason(error)
                await reloadLoops()
            }
        }
    }

    // MARK: - People in the room

    private func loadPeople() async {
        guard let sessionId = openSessionId else { return }
        do {
            let result: MeetingPeopleContextResult = try await core.request(
                "meeting_people_context", MeetingRequest.session(sessionId))
            people = result.rows
        } catch {
            people = []
        }
    }

    /// `previouslyTogetherRows`: only the people this meeting was not the
    /// first time with.
    var previouslyTogether: [MeetingPersonContextRow] {
        people.filter { $0.lastPriorMeeting != nil }
    }

    // MARK: - The notes a person types

    private func loadUserNotes() async {
        guard let sessionId = openSessionId else { return }
        do {
            let notes: MeetingUserNotes = try await core.request(
                "get_meeting_user_notes", MeetingRequest.session(sessionId))
            userNotes = notes
            notesBody = notes.body
            savedNoteRevision = notes.revision
            notesState = .idle
        } catch {
            notesState = .conflict
        }
    }

    /// Autosave, 1.2 seconds after the last keystroke.
    func typeNotes(_ text: String) {
        guard let notes = userNotes else { return }
        notesBody = text
        notesState = .unsaved
        notesTask?.cancel()
        notesTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingsStore.notesSettle)
            guard !Task.isCancelled, let self else { return }
            await self.persistNotes(text, template: notes.template)
        }
    }

    /// The blur React saves on: a pending change goes now rather than later.
    func flushNotes() {
        guard notesState == .unsaved, let notes = userNotes else { return }
        notesTask?.cancel()
        Task { await persistNotes(notesBody, template: notes.template) }
    }

    func choose(template: MeetingNotesTemplate) {
        guard let notes = userNotes, template != notes.template else { return }
        notesTask?.cancel()
        Task { await persistNotes(notesBody, template: template) }
    }

    @discardableResult
    private func persistNotes(_ body: String, template: MeetingNotesTemplate) async -> MeetingUserNotes? {
        guard let sessionId = openSessionId else { return nil }
        notesState = .saving
        do {
            let saved: MeetingUserNotes = try await core.request(
                "save_meeting_user_notes",
                MeetingRequest.userNotesSave(
                    sessionId, body: body, template: template,
                    expectedNoteRevision: savedNoteRevision)
            )
            savedNoteRevision = saved.revision
            userNotes = saved
            notesState = .saved
            return saved
        } catch {
            notesState = .conflict
            return nil
        }
    }

    /// "The saved word, plus when": what the band over the notes reads.
    var notesSavedLine: String? {
        guard let label = notesState.label else { return nil }
        guard notesState == .saved, let saved = userNotes else { return label }
        return "\(label) \(saved.updatedAtUtcMs.meetingDate.time)"
    }

    /// Write the notes again with what a person typed in front of the model.
    func reenhance() {
        guard let session = snapshot?.session, let notes = userNotes else { return }
        notesTask?.cancel()
        Task {
            enhancing = true
            defer { enhancing = false }
            do {
                let result: MeetingMutationResult = try await core.request(
                    "reenhance_meeting_with_notes",
                    MeetingRequest.reenhance(
                        session.sessionId, revision: session.revision, body: notesBody,
                        template: notes.template, expectedNoteRevision: savedNoteRevision)
                )
                receive(result.receipt)
                await loadUserNotes()
                await refreshSnapshot()
            } catch {
                self.error = reason(error)
            }
        }
    }

    /// "Catch me up": what has been said so far, in a handful of lines.
    func runCatchUp() {
        guard let sessionId = openSessionId else { return }
        Task {
            catchingUp = true
            defer { catchingUp = false }
            do {
                catchUp = try await core.request("meeting_catch_up", MeetingRequest.session(sessionId))
            } catch {
                catchUp = MeetingCatchUp(
                    state: .failed, bullets: [], throughOffsetNs: nil,
                    segmentCount: 0, provisional: false)
            }
        }
    }

    func dismissCatchUp() {
        catchUp = nil
    }

    // MARK: - Sending it on

    var hasLedger: Bool { snapshot?.currentLedger != nil }

    func openFollowUp() {
        guard let sessionId = openSessionId else { return }
        followUpOpen = true
        followUp = nil
        Task {
            drafting = true
            defer { drafting = false }
            do {
                followUp = try await core.request(
                    "meeting_follow_up_draft", MeetingRequest.followUpDraft(sessionId))
            } catch {
                self.error = reason(error)
                followUpOpen = false
            }
        }
    }

    func closeFollowUp() {
        followUpOpen = false
    }

    func copyFollowUp() {
        guard let draft = followUp else { return }
        copy(draft.body)
        followUpOpen = false
        notice = "The draft is on the clipboard."
    }

    /// Hand the draft to the mail client. Over the URL bound the core answers
    /// `clipboard`, which means the body goes on the clipboard and the message
    /// opens empty with a note saying so.
    func mailFollowUp() {
        guard let draft = followUp else { return }
        let body = draft.body
        Task {
            do {
                let mail: MeetingFollowUpMail = try await core.request(
                    "meeting_follow_up_mail",
                    MeetingRequest.followUpMail(
                        draft.sessionId, body: body,
                        overBoundNote: "The draft is on your clipboard: paste it here.")
                )
                if mail.body == .clipboard {
                    copy(body)
                }
                guard let url = URL(string: mail.url) else {
                    error = "The core answered with a mail address Sona cannot open."
                    return
                }
                NSWorkspace.shared.open(url)
                followUpOpen = false
                notice = mail.body == .clipboard
                    ? "Mail is open and the draft is on your clipboard."
                    : "Mail is open with the draft in it."
            } catch {
                self.error = reason(error)
            }
        }
    }

    private func copy(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    // MARK: - Saying what happened

    func dismissNotice() {
        notice = nil
    }

    func dismissError() {
        error = nil
    }

    /// One action, with the word for it while it runs.
    private func act(_ action: String, _ work: () async throws -> Void) async {
        pending = action
        defer { pending = nil }
        do {
            try await work()
        } catch let failure as CoreError {
            guard let remote = failure.remote(as: MeetingCommandError.self) else {
                error = failure.localizedDescription
                return
            }
            // A cancelled save panel is a person changing their mind.
            guard remote != .exportCancelled else { return }
            error = remote.label
            if remote == .staleRevision {
                await refreshSnapshot()
                await loadPage()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// The core's own word for a failure, in the sentence React shows for it.
    private func reason(_ error: Error) -> String {
        guard let failure = error as? CoreError else { return error.localizedDescription }
        return failure.remote(as: MeetingCommandError.self)?.label ?? failure.localizedDescription
    }
}

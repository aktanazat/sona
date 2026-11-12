import AppKit
import Foundation
import Observation
import UniformTypeIdentifiers

// MARK: - What the screens show

/// The three readings of one recorded meeting, in the order the review offers
/// them. Which one opens first depends on what the core has written: a ledger
/// wins, then notes, then the words themselves.
enum MeetingReviewTab: String, CaseIterable, Identifiable {
    case review
    case transcript
    case insights
    case ledger

    var id: String { rawValue }

    var title: String {
        switch self {
        case .review: "Review"
        case .transcript: "Transcript"
        case .insights: "Notes"
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

/// One save of the notes a person typed, captured whole at the moment it is
/// decided. It names its meeting rather than reading the open one, so a save
/// that starts as the page is left still lands on the meeting it belongs to,
/// and a slow answer from an earlier meeting cannot reach a later one's
/// editor.
private struct NotesDraft {
    let sessionId: MeetingSessionId
    let body: String
    let template: MeetingNotesTemplate
    /// One of the person's own templates, which is then the meeting's whole
    /// choice; `template` rides along as the fallback for when it is gone.
    let customTemplateId: String?
    let expectedNoteRevision: Int
}

/// The three things a person does to a loop or a commitment.
enum LoopChange {
    case resolve(dropped: Bool)
    case reopen
    case assign(personId: MeetingPersonId?)
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
    /// Each running meeting's latest progress. The core says when a run is
    /// over, whatever its outcome, and that removes the entry.
    private(set) var processing: [MeetingSessionId: MeetingProcessingUpdate] = [:]

    /// The cursor stack: one `created_at_utc_ms` per page walked back through.
    private var cursors: [Int64] = []
    var page: Int { cursors.count + 1 }

    // MARK: The deleted

    private(set) var trashOpen = false
    private(set) var trash: [MeetingTrashEntry] = []
    private(set) var trashLoading = false
    private(set) var restoring: MeetingDeletionJobId?
    private(set) var discarding: MeetingDeletionJobId?

    // MARK: Folders

    /// Every folder by name. A folder holds references, so the same meeting
    /// can sit in several.
    private(set) var folders: [MeetingFolder] = []
    private(set) var foldersLoading = true
    private(set) var foldersError: String?
    /// Why the last folder write did not land, for the sheet that made it.
    private(set) var folderError: String?
    private(set) var folderBusy = false
    /// The folder the list is narrowed to; nil lists every meeting.
    private(set) var folderId: MeetingFolderId?
    /// The meeting the filing sheet is open for.
    private(set) var filing: MeetingFiling?
    /// Saved prompts written about a meeting: the only kind a folder runs.
    private(set) var meetingPrompts: [SavedPrompt] = []
    private(set) var meetingPromptsLoading = false
    private(set) var meetingPromptsError: String?
    /// The revision the next folder write carries.
    @ObservationIgnored private var foldersRevision = 0

    // MARK: The open meeting

    private(set) var openSessionId: MeetingSessionId?
    private(set) var snapshot: MeetingReviewSnapshot?
    private(set) var reviewLoading = false
    /// Why the first read of the open meeting produced no page: the core's
    /// own words, kept apart from `error` so the page can offer another read
    /// instead of the sentence for a meeting that was deleted.
    private(set) var reviewFailure: String?
    private(set) var tab: MeetingReviewTab = .transcript
    private(set) var chosenTab: MeetingReviewTab?
    /// The part of the ledger's outcome bar picked, which narrows its threads.
    private(set) var ledgerOutcome: LedgerOutcome?
    private(set) var receipt: MeetingOperationReceipt?
    /// Every action still running, the newest last. One that ends leaves the
    /// others running, with the newest one's word on screen.
    private var running: [RunningAction] = []
    var pending: String? { running.last?.word }
    private(set) var analytics: MeetingAnalyticsSnapshot?
    private(set) var loops: [MeetingLoopRow]?
    private(set) var loopsBusy = false
    /// The open meeting's commitments that nothing in the planner cites yet.
    private(set) var untracked: MeetingUntrackedActionsRead = .reading
    private(set) var untrackedBusy = false
    /// Commitments set aside while this meeting is open, kept in view so the
    /// choice can be undone.
    private(set) var setAside: [MeetingLoopRow] = []
    private(set) var people: [MeetingPersonContextRow] = []
    /// Moves when the plans heard in the open meeting change, so every list
    /// of them reads them again.
    private(set) var heardPlansRevision = 0
    /// Moves when a document is added to the open meeting, so the card
    /// listing its documents reads them again.
    private(set) var documentsRevision = 0
    private(set) var transcriptQuery = ""
    /// The one voice the transcript is narrowed to, picked on the talk bar.
    private(set) var transcriptSpeaker: SpeakerId?
    private(set) var searchHits: [MeetingSearchHit]?
    private(set) var jump: SegmentJump?
    private(set) var focusedNoteId: MeetingNoteId?
    private(set) var originalWording = false
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
    private(set) var wrapOpen = false
    private(set) var drafting = false
    private(set) var savedLedgerPath: String?

    // MARK: What the store has to say

    /// The last thing the core refused, in the words React shows.
    private(set) var error: String?
    /// The last thing that worked, where React raises a toast.
    private(set) var notice: String?

    private let core: Core
    let audio: ConversationAudioPlayback
    /// Other meetings' lines that still read the way the last correction
    /// here fixed.
    let relisten: RelistenStore
    @ObservationIgnored private var listTask: Task<Void, Never>?
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var transcriptTask: Task<Void, Never>?
    @ObservationIgnored private var notesTask: Task<Void, Never>?
    @ObservationIgnored private var eventTask: Task<Void, Never>?
    @ObservationIgnored private var openTask: Task<Void, Never>?
    /// A departing save that did not land, kept for the next time that
    /// meeting is opened so the words are back in the editor rather than
    /// gone.
    @ObservationIgnored private var parkedDraft: NotesDraft?
    /// The revision each side read last, so a refresh that changed nothing
    /// does not re-ask for analytics or loops. Artifacts can be rewritten
    /// without the session's revision moving, so the generation counts too.
    @ObservationIgnored private var analyticsKey: String?
    @ObservationIgnored private var loopsKey: String?
    @ObservationIgnored private var untrackedKey: String?
    /// The note revision the core acknowledged: what the next save expects.
    @ObservationIgnored private var savedNoteRevision = 0
    @ObservationIgnored private var jumpNonce = 0

    init(core: Core) {
        self.core = core
        audio = ConversationAudioPlayback(core: core)
        relisten = RelistenStore(core: core)

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
        core.observe(CoreEvent.meetingProcessingProgress) { [weak self] line in
            guard let self, let payload: MeetingProcessingProgressPayload = try? Core.payload(line) else { return }
            self.processing[payload.sessionId] = payload.progress.map {
                MeetingProcessingUpdate(progress: $0, receivedAt: Date())
            }
        }
        core.observe(CoreEvent.meetingRemoved) { [weak self] line in
            guard let self else { return }
            let payload: MeetingEventPayload? = try? Core.payload(line)
            if let removed = payload?.sessionId, removed == self.openSessionId {
                self.closeReview()
            }
            self.reloadListSoon()
            Task { await self.loadFolders() }
        }
        core.observe(CoreEvent.meetingHeardPlansChanged) { [weak self] line in
            guard let self, let payload: MeetingEventPayload = try? Core.payload(line),
                  payload.sessionId == self.openSessionId else { return }
            self.heardPlansRevision += 1
        }
        core.observe("planning-changed-event") { [weak self] _ in
            guard let self, self.openSessionId != nil else { return }
            Task { await self.reloadUntracked() }
        }
        // A preflight is the live store's gate, not a page to read; opening it
        // here would put an empty review in front of the consent.
        core.observe(CoreEvent.meetingNavigationRequested) { [weak self] line in
            guard let self, let payload: MeetingNavigationPayload = try? Core.payload(line) else { return }
            switch payload.destination {
            case .list:
                self.closeReview()
            case .session:
                if let sessionId = payload.sessionId {
                    self.open(sessionId, atMilliseconds: payload.atMilliseconds, segmentId: payload.segmentId)
                }
            case .preflight:
                break
            }
        }
    }

    /// The first load: a page of meetings, the trend over them, and the
    /// folders. Progress heard from a core that has since gone is dropped.
    func start() async {
        processing = [:]
        await loadPage()
        await loadTrend()
        await loadFolders()
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

    /// The progress a meeting shows. Every step before the people runs while
    /// the meeting is still processing, so an update from one of those steps
    /// on a meeting that has moved on is from a run that ended unheard.
    func processingUpdate(_ sessionId: MeetingSessionId, phase: MeetingPhase) -> MeetingProcessingUpdate? {
        guard let update = processing[sessionId] else { return nil }
        return phase == .processing || update.progress.stage == .people ? update : nil
    }

    /// Coalesces a burst of events into one read of the list and of the
    /// totals the header shows.
    private func reloadListSoon() {
        listTask?.cancel()
        listTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingsStore.eventSettle)
            guard !Task.isCancelled else { return }
            await self?.loadPage()
            await self?.loadTrend()
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

    /// The line where the list would be: nothing recorded, nothing matching,
    /// or a folder with nothing in it.
    var emptyLine: String {
        if !committedQuery.isEmpty { return "No meetings match “\(committedQuery)”." }
        if let folder = chosenFolder {
            return "Nothing in “\(folder.name)” yet. Add a meeting from its menu."
        }
        return "No meetings yet. Record one and it lands here."
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
                    window: window,
                    folderId: folderId
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
        guard let expiresAtUtcMs = entry.expiresAtUtcMs else {
            return "Removed \(entry.deletedAt.short) by your retention setting · stays until you delete it"
        }
        let remaining = Double(expiresAtUtcMs - Int64(Date().timeIntervalSince1970 * 1000))
        let days = Int((remaining / Double(MeetingsStore.dayMs)).rounded(.up))
        let deleted = "Deleted \(entry.deletedAt.short)"
        if days <= 0 { return "\(deleted) · gone today" }
        return days == 1 ? "\(deleted) · gone tomorrow" : "\(deleted) · gone in \(days) days"
    }

    func restore(_ entry: MeetingTrashEntry) {
        Task {
            guard restoring == nil, discarding == nil else { return }
            error = nil
            restoring = entry.jobId
            defer { restoring = nil }
            do {
                let restored: MeetingSessionSnapshot = try await core.request(
                    "meeting_trash_restore", MeetingRequest.trashJob(entry.jobId))
                notice = "“\(restored.title)” is back."
                await loadTrash()
                await loadPage()
                await loadFolders()
            } catch {
                self.error = reason(error)
            }
        }
    }

    /// Remove one deleted meeting now instead of when its 30 days are up.
    /// There is no way back from this one.
    func deleteForever(_ entry: MeetingTrashEntry) {
        Task {
            guard restoring == nil, discarding == nil else { return }
            error = nil
            discarding = entry.jobId
            defer { discarding = nil }
            do {
                try await core.request("meeting_trash_delete_forever", MeetingRequest.trashJob(entry.jobId))
                notice = "“\(entry.title)” is gone for good."
                await loadTrash()
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
                    self.notice = "Deleted. It waits in the trash for 30 days."
                }
                await self.loadPage()
                await self.loadFolders()
            }
        }
    }

    // MARK: - Folders

    func loadFolders() async {
        do {
            let list: MeetingFolderList = try await core.request("meeting_folder_list")
            apply(list)
            foldersError = nil
        } catch {
            foldersError = reason(error)
        }
        foldersLoading = false
    }

    func retryFolders() {
        foldersLoading = true
        Task { await loadFolders() }
    }

    /// A folder list the core answered with. A chosen folder that is gone
    /// hands the list back to every meeting.
    private func apply(_ list: MeetingFolderList) {
        folders = list.folders
        foldersRevision = list.revision
        guard let folderId, !list.folders.contains(where: { $0.folderId == folderId }) else { return }
        self.folderId = nil
        cursors = []
        Task { await loadPage() }
    }

    var chosenFolder: MeetingFolder? {
        folderId.flatMap { chosen in folders.first { $0.folderId == chosen } }
    }

    /// Narrow the list to one folder's meetings, or nil for every meeting.
    func choose(folder: MeetingFolderId?) {
        guard folder != folderId else { return }
        folderId = folder
        cursors = []
        Task { await loadPage() }
    }

    /// Why a name cannot be a folder's, said before the core is asked: the
    /// core holds a name to 1 through 80 characters once trimmed, and to one
    /// no other folder has, ignoring case. `except` is the folder renamed.
    func folderNameProblem(_ raw: String, except: MeetingFolderId? = nil) -> String? {
        let name = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty { return "Give the folder a name." }
        if name.unicodeScalars.count > MeetingFolderLimit.nameCharacters {
            return "Keep the name to \(MeetingFolderLimit.nameCharacters) characters."
        }
        if name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) {
            return "Use a name without tabs, line breaks, or control characters."
        }
        let taken = folders.contains {
            $0.folderId != except && $0.name.caseInsensitiveCompare(name) == .orderedSame
        }
        return taken ? "Another folder has that name." : nil
    }

    func clearFolderError() {
        folderError = nil
    }

    /// Make a folder. Answers with it, so the sheet that asked can close or
    /// file a meeting in it.
    func createFolder(named raw: String) async -> MeetingFolder? {
        let name = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard await writeFolders(
            "meeting_folder_create", MeetingRequest.folderCreate(name: name, revision: foldersRevision))
        else { return nil }
        return folders.first { $0.name.caseInsensitiveCompare(name) == .orderedSame }
    }

    func renameFolder(_ folder: MeetingFolder, to raw: String) async -> Bool {
        let name = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return await writeFolders(
            "meeting_folder_rename",
            MeetingRequest.folderRename(folder.folderId, name: name, revision: foldersRevision))
    }

    /// Delete one folder. The meetings in it stay where they were.
    func deleteFolder(_ folder: MeetingFolder) {
        Task {
            let deleted = await writeFolders(
                "meeting_folder_delete", MeetingRequest.folderDelete(folder.folderId, revision: foldersRevision))
            if deleted {
                notice = "Deleted the folder “\(folder.name)”. Its meetings stay."
            }
        }
    }

    /// The template and the saved prompts a meeting filed here gets, replaced
    /// together.
    func setFolderDefaults(_ folder: MeetingFolder, template: MeetingNotesTemplate?, promptIds: [String]) async -> Bool {
        await writeFolders(
            "meeting_folder_set_defaults",
            MeetingRequest.folderDefaults(
                folder.folderId, template: template, promptIds: promptIds, revision: foldersRevision))
    }

    /// The saved prompts a folder can run. A prompt about a person or a
    /// series is left out: the core refuses one on a folder.
    func loadMeetingPrompts() async {
        meetingPromptsLoading = true
        defer { meetingPromptsLoading = false }
        do {
            let list: SavedPromptList = try await core.request("saved_prompt_list")
            meetingPrompts = list.prompts.filter { $0.target == .meeting }
            meetingPromptsError = nil
        } catch {
            meetingPromptsError = reason(error)
        }
    }

    /// One folder write. The list it answers with replaces the one here,
    /// refused or not, so the next write carries the revision the core is
    /// on.
    private func writeFolders(_ command: String, _ args: [String: JSONValue]) async -> Bool {
        guard !folderBusy else { return false }
        folderBusy = true
        defer { folderBusy = false }
        do {
            let result: MeetingFolderMutationResult = try await core.request(command, args)
            apply(result.folders)
            if let sessionId = filing?.sessionId { await loadFiling(sessionId) }
            guard result.receipt.result == .committed else {
                folderError = result.receipt.reasonCodes.contains(.staleRevision)
                    ? "Your folders changed in another window. Sona read them again, so try once more."
                    : result.receipt.refusal
                return false
            }
            folderError = nil
            return true
        } catch {
            folderError = reason(error)
            await loadFolders()
            if let sessionId = filing?.sessionId { await loadFiling(sessionId) }
            return false
        }
    }

    // MARK: - Filing one meeting

    /// Open the filing sheet for one meeting and read which folders hold it.
    func openFiling(_ sessionId: MeetingSessionId, title: String) {
        filing = MeetingFiling(sessionId: sessionId, title: title)
        folderError = nil
        Task {
            await loadFolders()
            await loadFiling(sessionId)
        }
    }

    func closeFiling() {
        filing = nil
        folderError = nil
    }

    func retryFiling() {
        guard let sessionId = filing?.sessionId else { return }
        filing?.error = nil
        Task { await loadFiling(sessionId) }
    }

    private func loadFiling(_ sessionId: MeetingSessionId) async {
        do {
            let ids: [MeetingFolderId] = try await core.request(
                "meeting_folders_for_session", MeetingRequest.session(sessionId))
            guard filing?.sessionId == sessionId else { return }
            filing?.folderIds = Set(ids)
            filing?.error = nil
        } catch {
            guard filing?.sessionId == sessionId else { return }
            filing?.error = reason(error)
        }
    }

    /// File the meeting the sheet is open for in one folder, or take it out.
    func setFiled(_ folder: MeetingFolder, _ filed: Bool) {
        Task { await file(folder, filed) }
    }

    /// Make a folder from the filing sheet and file the meeting in it.
    func createFolderAndFile(named name: String) async -> Bool {
        guard filing?.folderIds != nil, let folder = await createFolder(named: name) else { return false }
        return await file(folder, true)
    }

    @discardableResult
    private func file(_ folder: MeetingFolder, _ filed: Bool) async -> Bool {
        guard let sessionId = filing?.sessionId, filing?.folderIds != nil else { return false }
        let args = MeetingRequest.folderMembership(folder.folderId, sessionId: sessionId, revision: foldersRevision)
        let written = await writeFolders(
            filed ? "meeting_folder_add_meeting" : "meeting_folder_remove_meeting", args)
        guard written else { return false }
        if folderId == folder.folderId { await loadPage() }
        return true
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

    /// Every meeting as one CSV, one row each. The save panel comes first,
    /// so a person who changes their mind costs no export; then the core
    /// writes the rows and the file lands where they chose, the same way the
    /// vocabulary exports.
    func exportAllCsv() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.commaSeparatedText]
        panel.nameFieldStringValue = "sona-meetings.csv"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            await act("Exporting") {
                let csv: String = try await self.core.request("meeting_export_all_csv")
                try csv.write(to: url, atomically: true, encoding: .utf8)
                self.notice = "Every meeting is in \(url.lastPathComponent)."
            }
        }
    }

    // MARK: - Opening one meeting

    func open(
        _ sessionId: MeetingSessionId,
        atMilliseconds: UInt64? = nil,
        segmentId: TranscriptSegmentId? = nil,
        noteId: MeetingNoteId? = nil,
        outcome: LedgerOutcome? = nil
    ) {
        leaveOpenMeeting()
        openSessionId = sessionId
        snapshot = nil
        reviewFailure = nil
        analytics = nil
        analyticsKey = nil
        loops = nil
        loopsKey = nil
        untracked = .reading
        untrackedKey = nil
        setAside = []
        people = []
        receipt = nil
        chosenTab = if outcome != nil { .ledger } else if noteId != nil { .review } else { nil }
        tab = chosenTab ?? .transcript
        ledgerOutcome = outcome
        focusedNoteId = noteId
        transcriptQuery = ""
        transcriptSpeaker = nil
        searchHits = nil
        jump = nil
        originalWording = false
        newNote = ""
        userNotes = nil
        notesBody = ""
        notesState = .idle
        catchUp = nil
        followUp = nil
        wrapOpen = false
        reviewLoading = true
        openTask = Task { [weak self] in
            guard let self else { return }
            await refreshSnapshot()
            if isOpen(sessionId), !Task.isCancelled, atMilliseconds != nil || segmentId != nil {
                openSource(atMilliseconds: atMilliseconds, segmentId: segmentId)
            }
            await loadUserNotes()
            await loadPeople()
            if isOpen(sessionId) { reviewLoading = false }
        }
    }

    func closeReview() {
        leaveOpenMeeting()
        openSessionId = nil
        snapshot = nil
        reviewFailure = nil
        reviewLoading = false
        focusedNoteId = nil
        wrapOpen = false
    }

    /// The page in front of the reader is about to change. Every read that
    /// was still answering for the old meeting is dropped, and the notes they
    /// typed go to the core now rather than after a settle that will never
    /// come.
    private func leaveOpenMeeting() {
        openTask?.cancel()
        notesTask?.cancel()
        transcriptTask?.cancel()
        audio.stop()
        eventTask?.cancel()
        guard notesState == .unsaved, let draft = pendingDraft else { return }
        Task { await persist(draft) }
    }

    /// Whether an answer that arrives now is about the meeting on screen.
    private func isOpen(_ sessionId: MeetingSessionId) -> Bool {
        openSessionId == sessionId
    }

    func choose(tab: MeetingReviewTab) {
        chosenTab = tab
        self.tab = tab
    }

    func pick(outcome: LedgerOutcome?) {
        ledgerOutcome = outcome
    }

    /// `meeting_get`. A meeting that is no longer there closes the page; a
    /// read that failed for any other reason says so on the page, with the
    /// way to read again.
    func refreshSnapshot() async {
        guard let sessionId = openSessionId else { return }
        do {
            let next: MeetingReviewSnapshot = try await core.request(
                "meeting_get", MeetingRequest.session(sessionId))
            guard isOpen(sessionId) else { return }
            snapshot = next
            if next.session.transcriptDeleted {
                audio.stop()
                transcriptTask?.cancel()
                transcriptQuery = ""
                transcriptSpeaker = nil
                searchHits = nil
                jump = nil
            }
            reviewFailure = nil
            settleTab(next)
            await loadAnalytics(next)
            await loadLoops(next)
            await loadUntracked(next)
        } catch {
            guard isOpen(sessionId) else { return }
            if (error as? CoreError)?.remote(as: MeetingCommandError.self) == .notFound {
                closeReview()
                notice = "That meeting is no longer here."
                await loadPage()
                return
            }
            if snapshot == nil {
                reviewFailure = reason(error)
            } else {
                self.error = reason(error)
            }
        }
    }

    /// Read the meeting again after a first read that failed.
    func retryReview() {
        guard let sessionId = openSessionId else { return }
        reviewFailure = nil
        reviewLoading = true
        openTask?.cancel()
        openTask = Task { [weak self] in
            guard let self else { return }
            await refreshSnapshot()
            if userNotes == nil { await loadUserNotes() }
            if people.isEmpty { await loadPeople() }
            if isOpen(sessionId) { reviewLoading = false }
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
        if snapshot.session.phase == .reviewReady {
            tab = .review
        } else if snapshot.currentLedger != nil {
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

    /// Everyone who spoke the lines a receipt cites, in the order they spoke,
    /// for a receipt that quotes more than one person.
    func citedSpeakers(_ citations: [ArtifactCitation]) -> String? {
        guard let transcript = snapshot?.transcript else { return nil }
        var names: [String] = []
        for citation in citations {
            guard let segment = transcript.first(where: { $0.base.segmentId == citation.segmentId }),
                  let name = speakerNames[segment.assignedSpeakerId], !names.contains(name)
            else { continue }
            names.append(name)
        }
        return names.isEmpty ? nil : ListFormatter.localizedString(byJoining: names)
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
            let keep = (transcriptSpeaker == nil || segment.assignedSpeakerId == transcriptSpeaker)
                && (needle.isEmpty
                    || hits.contains(segment.base.segmentId)
                    || displayedText(segment).lowercased().contains(needle))
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
    /// A gap that starts more than a second after the run ends is a separate
    /// loss, as the core keeps it, and gets its own row and its own time.
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
                && Self.continues(runs.last, with: gap)
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

    /// The core's rule for one loss: a gap that starts within a second of
    /// where the run ends. Rows saved before the core recorded times have
    /// nothing to tell them apart, so they stay together.
    private static func continues(_ run: MeetingGapRun?, with gap: MeetingSourceGap) -> Bool {
        guard let start = gap.startOffsetNs, let reach = run?.endOffsetNs ?? run?.startOffsetNs else {
            return true
        }
        return start <= reach + 1_000_000_000
    }

    /// The transcript search, 250ms after the last keystroke, scoped to this
    /// meeting.
    func searchTranscript(_ text: String) {
        transcriptQuery = text
        searchHits = nil
        transcriptTask?.cancel()
        let needle = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !transcriptDeleted, !needle.isEmpty, let sessionId = openSessionId else { return }
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
        guard canJumpTo(segmentId) else { return }
        chosenTab = .transcript
        tab = .transcript
        transcriptQuery = ""
        transcriptSpeaker = nil
        searchHits = nil
        jumpNonce += 1
        jump = SegmentJump(segmentId: segmentId, nonce: jumpNonce)
    }

    func clearJump() {
        jump = nil
    }

    var transcriptDeleted: Bool { snapshot?.session.transcriptDeleted ?? false }
    var audioDeleted: Bool { snapshot?.session.audioDeleted ?? false }

    func canJumpTo(_ segmentId: TranscriptSegmentId) -> Bool {
        !transcriptDeleted && (snapshot?.transcript.contains { $0.base.segmentId == segmentId } ?? false)
    }

    /// Narrows the transcript to one voice and shows it, or lets it go.
    func showOnly(speaker: SpeakerId?) {
        transcriptSpeaker = speaker
        guard speaker != nil else { return }
        chosenTab = .transcript
        tab = .transcript
    }

    func citationAction(_ segmentId: TranscriptSegmentId) -> (() -> Void)? {
        guard canJumpTo(segmentId) else { return nil }
        return { [weak self] in self?.jumpTo(segmentId) }
    }

    func chooseOriginalWording(_ original: Bool) {
        originalWording = original
    }

    func displayedText(_ segment: TranscriptEffectiveSegment) -> String {
        originalWording ? segment.base.text : segment.text
    }

    func canPlay(_ segmentId: TranscriptSegmentId) -> Bool {
        guard !transcriptDeleted, !audioDeleted,
              let segment = snapshot?.transcript.first(where: { $0.base.segmentId == segmentId && !$0.removed })
        else { return false }
        return snapshot?.tracks.contains {
            $0.trackId == segment.base.trackId && $0.durableRecordCount > 0
                && ($0.firstOffsetNs ?? Int64.max) < segment.base.endOffsetNs
                && ($0.lastOffsetNs ?? 0) > segment.base.startOffsetNs
        } == true
    }

    func playContribution(_ segmentId: TranscriptSegmentId) {
        guard let sessionId = openSessionId, canPlay(segmentId) else { return }
        audio.play(sessionId: sessionId, segmentId: segmentId)
    }

    func playContribution(sessionId: String, segmentId: String) {
        audio.play(sessionId: sessionId, segmentId: segmentId)
    }

    private func segment(at offset: UInt64, uniqueOnly: Bool = false) -> TranscriptEffectiveSegment? {
        let moment = Int64(clamping: offset)
        var result: TranscriptEffectiveSegment?
        for segment in snapshot?.transcript ?? [] where !segment.removed
            && ((segment.base.startOffsetNs <= moment && segment.base.endOffsetNs > moment)
                || (offset.isMultiple(of: 1_000_000) && segment.base.startOffsetNs > moment
                    && segment.base.startOffsetNs / 1_000_000 == moment / 1_000_000)) {
            if !uniqueOnly { return segment }
            if result != nil { return nil }
            result = segment
        }
        return result
    }

    /// The segment where this speaker's turn starts at this moment: a
    /// previous turn still finishing there is not the one asked for.
    func segmentStarting(at offset: Int64, speaker: SpeakerId) -> TranscriptSegmentId? {
        snapshot?.transcript.first {
            !$0.removed && $0.base.startOffsetNs == offset && $0.assignedSpeakerId == speaker
        }?.base.segmentId
    }

    private func openSource(atMilliseconds: UInt64?, segmentId: TranscriptSegmentId?) {
        let conversion = atMilliseconds?.multipliedReportingOverflow(by: 1_000_000)
        guard conversion?.overflow != true else {
            notice = "Source timestamp is unavailable."
            return
        }
        if let segmentId {
            guard let segment = snapshot?.transcript.first(where: { $0.base.segmentId == segmentId && !$0.removed }),
                  segment.base.startOffsetNs >= 0, segment.base.endOffsetNs > segment.base.startOffsetNs
            else {
                notice = "Source words and recording are unavailable."
                return
            }
            let start = UInt64(segment.base.startOffsetNs)
            let requested = conversion?.partialValue ?? start
            let moment = requested / 1_000_000 == start / 1_000_000 ? max(requested, start) : requested
            guard moment >= start, moment < UInt64(segment.base.endOffsetNs) else {
                notice = "Source timestamp does not match these words."
                return
            }
            jumpTo(segmentId)
            guard canPlay(segmentId), let sessionId = openSessionId else {
                notice = "Recording is not retained."
                return
            }
            audio.play(sessionId: sessionId, segmentId: segmentId,
                startOffsetNs: moment > 4_000_000_000 ? moment - 4_000_000_000 : 0)
        } else if let offset = conversion?.partialValue, let segment = segment(at: offset) {
            if canPlay(segment.base.segmentId) { playAtOffset(offset) }
            else {
                jumpTo(segment.base.segmentId)
                notice = "Recording is not retained."
            }
        } else {
            notice = "Source words and recording are unavailable."
        }
    }

    func canPlayBookmark(_ offset: UInt64) -> Bool {
        guard !transcriptDeleted, !audioDeleted else { return false }
        let moment = Int64(clamping: offset)
        return snapshot?.tracks.contains {
            $0.durableRecordCount > 0
                && ($0.firstOffsetNs ?? .max) <= moment
                && ($0.lastOffsetNs ?? 0) > moment
        } == true
    }

    func playBookmark(_ offset: UInt64) {
        guard let sessionId = openSessionId, canPlayBookmark(offset) else { return }
        audio.playBookmark(sessionId: sessionId, offset: offset)
    }

    func playAtOffset(_ offset: UInt64) {
        guard let sessionId = openSessionId, let segment = segment(at: offset),
              canPlay(segment.base.segmentId) else { return }
        jumpTo(segment.base.segmentId)
        if self.segment(at: offset, uniqueOnly: true) == nil {
            notice = "Speakers overlap at this moment."
        }
        let contextStart = offset > 4_000_000_000 ? offset - 4_000_000_000 : 0
        audio.play(sessionId: sessionId, segmentId: segment.base.segmentId, startOffsetNs: contextStart)
    }

    // MARK: - Editing what the core wrote

    var editable: Bool { snapshot?.session.allows(.edit) ?? false }
    var busy: Bool { pending != nil }
    var canRegenerate: Bool { !transcriptDeleted && (snapshot?.session.allows(.regenerate) ?? false) }
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
        mutate(
            "Merging the speakers", "meeting_speaker_merge",
            then: {
                guard self.receipt?.refusal == nil, self.transcriptSpeaker == source else { return }
                self.transcriptSpeaker = target
            }
        ) { session in
            MeetingRequest.speakerMerge(
                session.sessionId, revision: session.revision, source: source, target: target)
        }
    }

    func editSegment(_ segment: TranscriptEffectiveSegment, text: String, removed: Bool = false) {
        let segmentId = segment.base.segmentId
        let fix = removed ? nil : snapshot.flatMap {
            RelistenFix.between(sessionId: $0.session.sessionId, segmentId: segmentId,
                                shown: segment.text, saved: text)
        }
        mutate(removed ? "Removing the line" : "Correcting the line", "meeting_segment_edit", then: {
            guard self.receipt?.refusal == nil else { return }
            if let fix { self.relisten.check(fix) } else { self.relisten.clear() }
        }) { session in
            MeetingRequest.segmentEdit(
                session.sessionId, revision: session.revision, segmentId: segmentId,
                replacementText: text, removed: removed)
        }
    }

    /// Takes several lines out of the transcript, or puts them back, as one
    /// edit.
    func setLinesRemoved(_ segmentIds: [TranscriptSegmentId], removed: Bool) {
        mutate(removed ? "Cutting the end" : "Putting the lines back", "meeting_segments_set_removed") { session in
            MeetingRequest.segmentsRemoved(
                session.sessionId, revision: session.revision, segmentIds: segmentIds, removed: removed)
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
        guard let session = snapshot?.session else { return }
        Task {
            await act("Rewriting the notes") { try await self.requestRewrite(session) }
        }
    }

    /// Asks the core to write the notes again. A rewrite that wrote nothing
    /// says why, read from the status the core wrote for the meeting that
    /// was pressed, and whether its earlier notes stay.
    private func requestRewrite(_ session: MeetingSessionSnapshot) async throws {
        do {
            let result: MeetingMutationResult = try await core.request(
                "meeting_artifacts_regenerate",
                MeetingRequest.wrap(MeetingRequest.mutation(session.sessionId, revision: session.revision))
            )
            receive(result.receipt)
        } catch let failure as CoreError where failure.remote(as: MeetingCommandError.self) == .engineFailure {
            let pressed: MeetingReviewSnapshot? = try? await core.request(
                "meeting_get", MeetingRequest.session(session.sessionId))
            let why = pressed?.session.processingStatus.cause?.label ?? "the model did not finish them"
            let kept = pressed?.readableArtifact?.content != nil
            let which = isOpen(session.sessionId) ? "" : " for \(session.title)"
            error = "No new notes were written\(which) because \(why)."
                + (kept ? " The earlier notes are still there." : "")
            await refreshSnapshot()
            return
        }
        await refreshSnapshot()
        await loadPage()
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

    /// Add a PDF or a text document to the open meeting. The operation id is
    /// the core's idempotency key: the same add twice is one document.
    func addDocument() {
        guard let sessionId = openSessionId else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.message = "A PDF or text document for this meeting"
        panel.allowedContentTypes = DocumentMedia.extensions.compactMap { UTType(filenameExtension: $0) }
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            await act("Adding the document") {
                let request: [String: JSONValue] = [
                    "path": .string(url.path),
                    "operation_id": .string(UUID().uuidString),
                    "session_id": .string(sessionId),
                ]
                let _: DocumentMutationResult = try await self.core.request("doc_ingest", ["request": request])
                self.documentsRevision += 1
            }
        }
    }

    /// A document was added to the open meeting from somewhere other than
    /// `addDocument`, so its lists read again.
    func documentsChanged() {
        documentsRevision += 1
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
        let sessionId = snapshot.session.sessionId
        let read: MeetingAnalyticsSnapshot? = try? await core.request(
            "get_meeting_analytics", MeetingRequest.session(sessionId))
        guard isOpen(sessionId) else { return }
        analytics = read
    }

    /// The talk strip only reads when somebody said something.
    var talk: MeetingTalkMetrics? {
        guard let talk = analytics?.analytics.talk, talk.segmentCount > 0 else { return nil }
        return talk
    }

    var trackers: [MeetingTrackerResult] { analytics?.analytics.trackers ?? [] }

    /// The voices that said something, loudest first: a thin bar each.
    var talkShares: [SpeakerTalkShare] {
        (talk?.speakers ?? [])
            .filter { $0.turnCount > 0 }
            .sorted { $0.sharePermille > $1.sharePermille }
    }

    /// `formatPatience`: milliseconds under a second, one decimal above it.
    /// A room of one has no switch to wait through, so it reads nothing.
    var patience: String? {
        guard talkShares.count > 1, let gap = talk?.medianSwitchGapMs else { return nil }
        return gap < 1_000 ? "\(gap)ms" : String(format: "%.1fs", Double(gap) / 1_000)
    }

    /// How often the room changed hands, while two voices each hold at least
    /// one turn in twenty. Below that one side is listening, and a count of
    /// handovers to a listener says nothing.
    var handovers: Int? {
        guard let talk, talk.interactionCount > 0 else { return nil }
        let talkers = talk.speakers.filter { $0.turnCount > 0 && $0.turnCount * 20 >= talk.turnCount }
        return talkers.count > 1 ? talk.interactionCount : nil
    }

    // MARK: - Loops and commitments

    private func loadLoops(_ snapshot: MeetingReviewSnapshot) async {
        guard loopsKey != MeetingsStore.key(snapshot) else { return }
        loopsKey = MeetingsStore.key(snapshot)
        await reloadLoops()
    }

    private func reloadLoops() async {
        guard let sessionId = openSessionId else { return }
        let read: MeetingLoopsResult? = try? await core.request(
            "meeting_loops", MeetingRequest.session(sessionId))
        guard isOpen(sessionId) else { return }
        loops = read?.rows ?? []
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
            await reloadUntracked()
        }
    }

    // MARK: - Commitments not in the planner yet

    private func loadUntracked(_ snapshot: MeetingReviewSnapshot) async {
        guard untrackedKey != MeetingsStore.key(snapshot) else { return }
        untrackedKey = MeetingsStore.key(snapshot)
        await reloadUntracked()
    }

    private func reloadUntracked() async {
        guard let sessionId = openSessionId else { return }
        do {
            let read: MeetingUntrackedActions = try await core.request(
                "meeting_untracked_actions", MeetingRequest.session(sessionId))
            guard isOpen(sessionId) else { return }
            untracked = .read(MeetingUntrackedActions(
                actions: read.actions.filter { action in !setAside.contains { $0.loopId == action.loopId } },
                projects: read.projects))
        } catch {
            guard isOpen(sessionId) else { return }
            untracked = .unavailable(reason(error))
        }
    }

    /// Read the commitments again after a read that failed.
    func retryUntracked() {
        untracked = .reading
        Task { await reloadUntracked() }
    }

    /// One planner task for the commitment, in the project picked. The row
    /// leaves and the toast names where it went.
    func track(_ action: MeetingLoopRow, in project: PlanningProject) {
        untrackedBusy = true
        Task {
            defer { untrackedBusy = false }
            do {
                let _: PlanningTask = try await core.request(
                    "planning_track_action", MeetingRequest.trackAction(action.loopId, projectId: project.id))
                drop(action)
                notice = "Tracked in \(project.title)."
            } catch {
                self.error = reason(error)
                await reloadUntracked()
            }
        }
    }

    /// The commitment is not work, so it stops asking to be tracked. It stays
    /// in view, set aside, until the meeting is closed.
    func setAside(_ action: MeetingLoopRow) {
        untrackedBusy = true
        Task {
            defer { untrackedBusy = false }
            do {
                try await core.request("planning_dismiss_action", MeetingRequest.dismissAction(action.loopId))
                drop(action)
                setAside.append(action)
            } catch {
                self.error = reason(error)
                await reloadUntracked()
            }
        }
    }

    /// Asks about a set-aside commitment again.
    func undoSetAside(_ action: MeetingLoopRow) {
        untrackedBusy = true
        Task {
            defer { untrackedBusy = false }
            do {
                try await core.request("planning_restore_action", MeetingRequest.dismissAction(action.loopId))
                setAside.removeAll { $0.loopId == action.loopId }
            } catch {
                self.error = reason(error)
            }
            await reloadUntracked()
        }
    }

    private func drop(_ action: MeetingLoopRow) {
        guard case let .read(read) = untracked else { return }
        untracked = .read(MeetingUntrackedActions(
            actions: read.actions.filter { $0.loopId != action.loopId }, projects: read.projects))
    }

    // MARK: - People in the room

    private func loadPeople() async {
        guard let sessionId = openSessionId else { return }
        let read: MeetingPeopleContextResult? = try? await core.request(
            "meeting_people_context", MeetingRequest.session(sessionId))
        guard isOpen(sessionId) else { return }
        people = read?.rows ?? []
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
            guard isOpen(sessionId) else { return }
            userNotes = notes
            savedNoteRevision = notes.revision
            // A save that did not land when this meeting was last left goes
            // back into the editor, unsaved, so the autosave carries it.
            if let parked = parkedDraft, parked.sessionId == sessionId, parked.body != notes.body {
                parkedDraft = nil
                notesBody = parked.body
                notesState = .unsaved
                scheduleNotesSave()
            } else {
                notesBody = notes.body
                notesState = .idle
            }
        } catch {
            guard isOpen(sessionId) else { return }
            notesState = .conflict
        }
    }

    /// What a save right now would send: the open meeting, the words in the
    /// editor, its template, and the revision the core last acknowledged.
    private var pendingDraft: NotesDraft? {
        guard let sessionId = openSessionId, let notes = userNotes else { return nil }
        return NotesDraft(
            sessionId: sessionId, body: notesBody, template: notes.template,
            customTemplateId: notes.customTemplateId, expectedNoteRevision: savedNoteRevision)
    }

    /// Autosave, 1.2 seconds after the last keystroke.
    func typeNotes(_ text: String) {
        guard userNotes != nil else { return }
        notesBody = text
        notesState = .unsaved
        scheduleNotesSave()
    }

    private func scheduleNotesSave() {
        notesTask?.cancel()
        notesTask = Task { [weak self] in
            try? await Task.sleep(for: MeetingsStore.notesSettle)
            guard !Task.isCancelled, let self, let draft = pendingDraft else { return }
            await persist(draft)
        }
    }

    /// The blur React saves on: a pending change goes now rather than later.
    func flushNotes() {
        guard notesState == .unsaved, let draft = pendingDraft else { return }
        notesTask?.cancel()
        Task { await persist(draft) }
    }

    /// The meeting's own template: a built-in or one of the person's own.
    /// Picking one rewrites the notes with it while the meeting can still be
    /// rewritten, which is what choosing a template is for; a meeting that
    /// cannot be rewritten only remembers the choice.
    func choose(template choice: MeetingTemplateChoice<MeetingNotesTemplate>) {
        guard let notes = userNotes, let sessionId = openSessionId,
              choice != MeetingTemplateChoice(builtIn: notes.template, customTemplateId: notes.customTemplateId)
        else { return }
        let draft = NotesDraft(
            sessionId: sessionId, body: notesBody, template: choice.builtIn ?? notes.template,
            customTemplateId: choice.customTemplateId, expectedNoteRevision: savedNoteRevision)
        if canRegenerate {
            rewrite(draft)
        } else {
            notesTask?.cancel()
            Task { await persist(draft) }
        }
    }

    /// One save, addressed to the meeting the draft names. The editor only
    /// learns the outcome while that meeting is still the open one; a save
    /// for a meeting the reader has left either lands quietly or is parked
    /// for their return.
    @discardableResult
    private func persist(_ draft: NotesDraft) async -> MeetingUserNotes? {
        if isOpen(draft.sessionId) { notesState = .saving }
        do {
            let saved: MeetingUserNotes = try await core.request(
                "save_meeting_user_notes",
                MeetingRequest.userNotesSave(
                    draft.sessionId, body: draft.body, template: draft.template,
                    customTemplateId: draft.customTemplateId, expectedNoteRevision: draft.expectedNoteRevision)
            )
            guard isOpen(draft.sessionId) else { return saved }
            savedNoteRevision = saved.revision
            userNotes = saved
            // Words typed while the save was out are not saved yet.
            notesState = notesBody == draft.body ? .saved : .unsaved
            return saved
        } catch {
            if isOpen(draft.sessionId) {
                notesState = .conflict
            } else {
                parkedDraft = draft
            }
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
    /// The words are saved first, on their own, so the editor holds the
    /// revision the core acknowledged whether or not the model then answers:
    /// a rewrite that fails must not leave the next keystroke's save
    /// refused as stale.
    func reenhance() {
        guard let draft = pendingDraft else { return }
        rewrite(draft)
    }

    /// Saves `draft`, then writes the notes again from it: the words and the
    /// template it names are what the model reads.
    private func rewrite(_ draft: NotesDraft) {
        guard snapshot?.session != nil else { return }
        notesTask?.cancel()
        Task {
            enhancing = true
            defer { enhancing = false }
            guard await persist(draft) != nil, isOpen(draft.sessionId) else { return }
            await act("Rewriting the notes") {
                guard let session = self.snapshot?.session else { return }
                try await self.requestRewrite(session)
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

    var hasLedger: Bool { snapshot?.readableLedger != nil }

    func heardPlansChanged() {
        heardPlansRevision += 1
    }

    /// The wrap opens on what the meeting already holds. The follow-up is
    /// written only when asked for, since writing it may reach the model.
    func openWrap() {
        guard openSessionId != nil else { return }
        wrapOpen = true
        followUp = nil
        error = nil
    }

    func closeWrap() {
        wrapOpen = false
    }

    func draftFollowUp() {
        guard let sessionId = openSessionId, !drafting else { return }
        followUp = nil
        error = nil
        drafting = true
        Task {
            defer { drafting = false }
            do {
                let draft: MeetingFollowUpDraft = try await core.request(
                    "meeting_follow_up_draft", MeetingRequest.followUpDraft(sessionId))
                guard isOpen(sessionId) else { return }
                followUp = draft
            } catch {
                guard isOpen(sessionId) else { return }
                self.error = reason(error)
            }
        }
    }

    func copyFollowUp() {
        guard let draft = followUp else { return }
        copy(draft.body)
        wrapOpen = false
        notice = "The draft is on the clipboard."
    }

    /// With "Open drafts in Mail" on, Mail gets the complete draft in a
    /// visible compose window. Otherwise the default mail app opens from a
    /// mailto link, and a draft too long for the link goes on the clipboard.
    /// Nothing is ever sent.
    func mailFollowUp() {
        guard let draft = followUp, !draft.body.isEmpty, pending == nil else { return }
        let opening = RunningAction(word: "Opening Mail")
        running.append(opening)
        error = nil
        Task {
            defer { running.removeAll { $0.id == opening.id } }
            do {
                let preferences: MeetingPrepPreferences = try await core.request("meeting_prep_preferences_get")
                if preferences.mailComposeEnabled {
                    let mail: MeetingMailContext = try await core.request(
                        "meeting_follow_up_compose",
                        ["sessionId": JSONValue.string(draft.sessionId), "body": .string(draft.body)])
                    guard mail.state == .ready else {
                        error = mail.state.detail
                        return
                    }
                    wrapOpen = false
                    notice = "Mail is open with the full draft. Review it before sending."
                    return
                }
                let mail: MeetingFollowUpMail = try await core.request(
                    "meeting_follow_up_mail",
                    MeetingRequest.followUpMail(
                        draft.sessionId, body: draft.body,
                        overBoundNote: "The follow-up is on the clipboard. Paste it here."))
                if mail.body == .clipboard { copy(draft.body) }
                guard let url = URL(string: mail.url), NSWorkspace.shared.open(url) else {
                    error = "No mail app opened. Copy the draft instead."
                    return
                }
                wrapOpen = false
                notice = mail.body == .clipboard
                    ? "The draft is on the clipboard. Paste it into the new email."
                    : "Your mail app is open with the draft. Review it before sending."
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

    /// One action while it runs, told apart from another with the same word.
    private struct RunningAction {
        let id = UUID()
        let word: String
    }

    /// One action, with the word for it while it runs.
    private func act(_ action: String, _ work: () async throws -> Void) async {
        let mine = RunningAction(word: action)
        running.append(mine)
        error = nil
        defer { running.removeAll { $0.id == mine.id } }
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

/// The meeting the filing sheet is open for, and the folders it sits in once
/// they are read. Neither set nor error is the read still running.
struct MeetingFiling: Equatable {
    let sessionId: MeetingSessionId
    let title: String
    var folderIds: Set<MeetingFolderId>?
    var error: String?
}

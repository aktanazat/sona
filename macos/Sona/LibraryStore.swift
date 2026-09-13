import AVFoundation
import AppKit
import Foundation
import Observation

extension CoreEvent {
    /// The core says settings changed without saying which, so the page
    /// re-reads the two it owns.
    static let librarySettingsChanged = "settings-changed"
}

/// Everything the Library reads or writes: the log and its paging, the search
/// over it, the totals and the trend above it, the receipts under a row, the
/// stored recording a row can play, and how much of any of it is kept.
///
/// The transcription pipeline owns every history write. This store asks for a
/// page, then mirrors the core's own events into the page on screen, so a
/// dictation made while the Library is open lands in it without a reload.
@MainActor
@Observable
final class LibraryStore {
    /// One page of the log. The feed asks for the next one by naming the last
    /// row it holds.
    static let pageSize = 30
    /// The most entries the core will keep, as the old number field allowed.
    static let limitCeiling = 1000
    private static let searchPause = Duration.milliseconds(200)
    private static let copiedPause = Duration.seconds(2)
    private static let tick = Duration.milliseconds(50)

    // MARK: The log

    /// The rows on screen, newest first.
    private(set) var rows: [HistoryRow] = []
    /// The same rows in local-day groups, which is how the feed draws them.
    private(set) var days: [HistoryDay] = []
    private(set) var phase: LibraryPhase = .loading
    private(set) var hasMore = false
    /// The search the rows on screen answer to.
    private(set) var activeQuery = ""
    /// What is in the search field. Typing settles into `activeQuery`.
    var query = "" {
        didSet { scheduleSearch() }
    }
    /// Which field of a row the log is read in.
    var textView: LibraryTextView = .processed
    /// The one open row. The store owns it because a row cannot close its
    /// neighbour, and thirty open rows is not a log any more.
    private(set) var expanded: Int64?
    /// The rows whose transcription is being run again, and the rows being
    /// deleted: both change what the row is allowed to say and do.
    private(set) var retrying: Set<Int64> = []
    private(set) var deleting: Set<Int64> = []
    /// The row that was copied in the last two seconds.
    private(set) var copied: Int64?

    // MARK: Totals and trend

    private(set) var stats: HistoryStats?
    private(set) var statsLoading = true
    private(set) var statsFailed = false
    private(set) var trend: TrendProjection?
    var trendRange: TrendRange = .week {
        didSet {
            guard trendRange != oldValue else { return }
            Task { await loadTrend() }
        }
    }

    // MARK: Receipts

    /// What is known about each visible row's receipts. A row asks for its own
    /// when it is drawn, and rows that leave the page drop theirs.
    private(set) var receipts: [Int64: ReceiptLoad] = [:]

    // MARK: Playback

    /// The row whose recording is loaded, and the row whose bytes are still
    /// being read. One recording plays at a time.
    private(set) var playingId: Int64?
    private(set) var loadingAudio: Int64?
    private(set) var playing = false
    private(set) var position: TimeInterval = 0
    private(set) var duration: TimeInterval = 0

    // MARK: What is kept

    private(set) var limit = 0
    private(set) var retention: LibraryRetention = .never
    private(set) var storage: HistoryStorageState?
    private(set) var savingRetention = false

    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var pageRequest = 0
    @ObservationIgnored private var statsRequest = 0
    @ObservationIgnored private var paging = false
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var copiedTask: Task<Void, Never>?
    @ObservationIgnored private var ticker: Task<Void, Never>?
    @ObservationIgnored private var player: AVAudioPlayer?

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.historyUpdate) { [weak self] line in self?.applyHistory(line) }
        // The startup unlock. A read the core refused while the database was
        // still locked is asked again here, once storage says what it is.
        core.observe(CoreEvent.historyStorage) { [weak self] line in
            guard let self else { return }
            self.storage = try? Core.payload(line)
            if self.phase == .error { self.reload() }
            Task { await self.refreshStats() }
            Task { await self.loadTrend() }
        }
        core.observe(CoreEvent.librarySettingsChanged) { [weak self] _ in
            Task { await self?.loadSettings() }
        }
    }

    /// The first read: the page the reader sees, and the four smaller answers
    /// around it, all in flight together.
    func start() async {
        async let page: Void = fetchPage(cursor: nil)
        async let totals: Void = refreshStats()
        async let settings: Void = loadSettings()
        async let storageState: Void = loadStorage()
        async let activity: Void = loadTrend()
        _ = await (page, totals, settings, storageState, activity)
    }

    // MARK: - Reading the log

    /// The first page again, under whichever search is on screen.
    func reload() {
        Task { await fetchPage(cursor: nil) }
    }

    /// The next page, from the last row held. Called by the foot of the feed
    /// when it comes into view, and by the button beside it.
    func loadMore() {
        guard hasMore, phase == .ready || phase == .pagingError, let last = rows.last else { return }
        Task { await fetchPage(cursor: last.id) }
    }

    func clearSearch() {
        query = ""
    }

    private func scheduleSearch() {
        searchTask?.cancel()
        let typed = query
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: Self.searchPause)
            guard !Task.isCancelled, let self, self.query == typed, self.activeQuery != typed else { return }
            self.activeQuery = typed
            await self.fetchPage(cursor: nil)
        }
    }

    /// One page, under the active search. Only the newest request may write,
    /// so a slow page for an abandoned search never lands on the one being
    /// read. A failure carries no message: the core logs the cause, and the
    /// feed has one state for every one of them.
    private func fetchPage(cursor: Int64?) async {
        let append = cursor != nil
        if append, paging { return }
        pageRequest += 1
        let request = pageRequest
        paging = true
        if append {
            if phase == .ready || phase == .pagingError { phase = .paging }
        } else {
            rows = []
            days = []
            hasMore = false
            phase = .loading
        }
        let trimmed = activeQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            let page: HistoryPage = trimmed.isEmpty
                ? try await core.request("get_history_entries", HistoryPageParams(cursor: cursor, limit: Self.pageSize))
                : try await core.request(
                    "search_history_entries",
                    HistorySearchParams(query: trimmed, cursor: cursor, limit: Self.pageSize))
            guard request == pageRequest else { return }
            if append {
                let known = Set(rows.map(\.id))
                rows += page.entries.filter { !known.contains($0.id) }
            } else {
                rows = page.entries
            }
            hasMore = page.hasMore
            phase = .ready
            rebuildDays()
            evictReceipts()
        } catch {
            guard request == pageRequest else { return }
            phase = append ? .pagingError : .error
        }
        if request == pageRequest { paging = false }
    }

    /// The rows in local-day groups, with the runs that produced no words held
    /// apart: a day of those is one line to open, not one full row each.
    private func rebuildDays() {
        let calendar = Calendar.current
        var order: [Date] = []
        var grouped: [Date: [HistoryRow]] = [:]
        for row in rows {
            let day = calendar.startOfDay(for: row.item.date)
            if grouped[day] == nil { order.append(day) }
            grouped[day, default: []].append(row)
        }
        days = order.map { day in
            let items = grouped[day] ?? []
            return HistoryDay(
                id: day,
                heading: day.relativeDay,
                spoken: items.filter { !$0.silent },
                silent: items.filter(\.silent))
        }
    }

    // MARK: - The core's own writes

    /// A history write the pipeline announced, mirrored into the page on
    /// screen. The action comes from the shared `HistoryUpdate`; the entry is
    /// read a second time through `HistoryRow`, which keeps the three fields
    /// the shared mirror leaves out. A frame that will not decode as a row is
    /// still a frame that says something changed, so the page reloads.
    private func applyHistory(_ line: Data) {
        guard let update: HistoryUpdate = try? Core.payload(line) else { return }
        let carried: HistoryRowUpdate? = try? Core.payload(line)
        let row = carried?.entry
        switch update {
        case .added:
            // A new entry has not been matched against an active search, so it
            // joins the unfiltered list only. The next search asks the core.
            if activeQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                if let row {
                    rows.removeAll { $0.id == row.id }
                    rows.insert(row, at: 0)
                    rebuildDays()
                } else {
                    reload()
                }
            }
        case .updated:
            if let row {
                guard let index = rows.firstIndex(where: { $0.id == row.id }) else { break }
                rows[index] = row
                rebuildDays()
                // A retry or a reprocess writes a new receipt for this row.
                receipts[row.id] = nil
                Task { await loadReceipts(for: row.id) }
            } else {
                reload()
            }
        case let .deleted(id):
            rows.removeAll { $0.id == id }
            deleting.remove(id)
            receipts[id] = nil
            if expanded == id { expanded = nil }
            if playingId == id || loadingAudio == id { stopPlayback() }
            rebuildDays()
        case let .toggled(id):
            guard let index = rows.firstIndex(where: { $0.id == id }) else { break }
            rows[index].item.saved.toggle()
            rebuildDays()
        }
        Task {
            await refreshStats()
            await loadTrend()
        }
    }

    // MARK: - Totals and trend

    /// All-time totals. Only the newest read may write, a failure clears what
    /// was there, and a late answer never overwrites a fresh one.
    func refreshStats() async {
        statsRequest += 1
        let request = statsRequest
        statsLoading = true
        statsFailed = false
        do {
            let totals: HistoryStats = try await core.request("get_history_stats")
            if request == statsRequest { stats = totals }
        } catch {
            if request == statsRequest {
                stats = nil
                statsFailed = true
            }
        }
        statsLoading = false
    }

    /// The activity band: one bar per local day in the chosen window. A window
    /// the core cannot project simply leaves the section out.
    func loadTrend() async {
        trend = try? await core.request("get_history_trend", TrendParams(request: TrendRequest(range: trendRange)))
    }

    // MARK: - Receipts

    /// The receipts for one row, read once per row per page. The row asks when
    /// it is drawn, which is what keeps the read to the rows in front of the
    /// reader.
    func loadReceipts(for id: Int64) async {
        guard receipts[id] == nil else { return }
        receipts[id] = .loading
        do {
            let list: [HistoryRunReceipt] = try await core.request(
                "get_history_run_receipts", HistoryEntryParam(historyId: id))
            receipts[id] = .ready(list.sorted { $0.completedAtMs > $1.completedAtMs })
        } catch {
            receipts[id] = .failed
        }
    }

    /// The run that finished last, which is the one that describes the row.
    func latestReceipt(for id: Int64) -> HistoryRunReceipt? {
        guard case let .ready(list) = receipts[id] else { return nil }
        return list.first
    }

    /// Rows that left the page drop their receipts; the rows that arrived ask
    /// for their own.
    private func evictReceipts() {
        let visible = Set(rows.map(\.id))
        receipts = receipts.filter { visible.contains($0.key) }
    }

    // MARK: - What a row can do

    func toggleExpanded(_ id: Int64) {
        if expanded == id {
            expanded = nil
            // The player belongs to the open row; closing it stops the sound.
            if playingId == id || loadingAudio == id { stopPlayback() }
        } else {
            expanded = id
        }
    }

    func toggleSaved(_ id: Int64) {
        call { [self] in
            try await core.request("toggle_history_entry_saved", HistoryIdParam(id: id))
        }
    }

    /// The words on screen, on the clipboard. The row says "Copied" for two
    /// seconds afterwards.
    func copy(_ id: Int64, text: String) {
        guard !text.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        copied = id
        copiedTask?.cancel()
        copiedTask = Task { [weak self] in
            try? await Task.sleep(for: Self.copiedPause)
            guard !Task.isCancelled else { return }
            self?.copied = nil
        }
    }

    /// Deletes the entry and its recording. The row leaves on the core's own
    /// deleted event; a refused delete puts it back rather than leaving a
    /// ghost.
    func delete(_ id: Int64) {
        deleting.insert(id)
        Task { [self] in
            do {
                try await core.request("delete_history_entry", HistoryIdParam(id: id))
                error = nil
            } catch {
                deleting.remove(id)
                self.error = error.localizedDescription
            }
        }
    }

    /// Runs the stored recording through the active mode again. The row reads
    /// "Transcribing…" while the call is out.
    func retryTranscription(_ id: Int64) {
        guard !retrying.contains(id) else { return }
        retrying.insert(id)
        Task { [self] in
            do {
                try await core.request("retry_history_entry_transcription", HistoryIdParam(id: id))
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
            retrying.remove(id)
        }
    }

    /// The modes the Process again dialog offers. Read when the dialog opens,
    /// because no row needs the list until it is asked for.
    func reprocessModes() async throws -> ReprocessModeList {
        try await core.request("get_modes")
    }

    /// Runs a stored recording through a different mode. The original entry is
    /// kept; the result arrives as a new row pointing back at it.
    func reprocess(_ id: Int64, modeId: String) async throws {
        try await core.request("reprocess_history_entry", ReprocessParams(id: id, modeId: modeId))
    }

    func openRecordingsFolder() {
        call { [self] in
            try await core.request("open_recordings_folder")
        }
    }

    // MARK: - Playback

    /// Whether a receipt says the row is worth a player. A receipt is the only
    /// thing that can prove a recording holds audio, and rows written before
    /// receipts existed keep the player: taking it away on a guess is the
    /// larger error.
    func playable(_ id: Int64) -> Bool {
        guard let receipt = latestReceipt(for: id) else { return true }
        return receipt.hasAudio && (receipt.durationMs ?? 0) > 0
    }

    /// The length the row already knows, before any audio is read.
    func statedLength(_ id: Int64) -> TimeInterval? {
        guard let milliseconds = latestReceipt(for: id)?.durationMs else { return nil }
        return TimeInterval(milliseconds) / 1000
    }

    /// Play, pause, or read the recording first. The bytes arrive from the
    /// core in bounded chunks and are handed to one player; a second row
    /// takes the player from the first.
    func togglePlayback(for id: Int64) {
        if playingId == id, let player {
            if player.isPlaying {
                player.pause()
                playing = false
                ticker?.cancel()
            } else {
                resume()
            }
            return
        }
        stopPlayback()
        loadingAudio = id
        Task { [self] in
            do {
                let data = try await audio(for: id)
                guard loadingAudio == id else { return }
                let made: AVAudioPlayer
                do {
                    made = try AVAudioPlayer(data: data)
                } catch {
                    throw LibraryFault.audioUnplayable(error.localizedDescription)
                }
                made.prepareToPlay()
                player = made
                playingId = id
                duration = made.duration
                position = 0
                loadingAudio = nil
                error = nil
                resume()
            } catch {
                if loadingAudio == id { loadingAudio = nil }
                self.error = error.localizedDescription
            }
        }
    }

    /// Moves the head. `fraction` is where along the recording it lands.
    func seek(_ fraction: Double) {
        guard let player, duration > 0 else { return }
        let time = min(max(fraction, 0), 1) * duration
        player.currentTime = time
        position = time
    }

    func stopPlayback() {
        ticker?.cancel()
        ticker = nil
        player?.stop()
        player = nil
        playingId = nil
        loadingAudio = nil
        playing = false
        position = 0
        duration = 0
    }

    private func resume() {
        guard let player else { return }
        guard player.play() else {
            error = LibraryFault.audioUnplayable("the audio device refused it").localizedDescription
            return
        }
        playing = true
        ticker?.cancel()
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.tick)
                guard !Task.isCancelled, let self, let player = self.player else { return }
                guard player.isPlaying else {
                    // The recording ran out. The head stays at the end until
                    // it is moved, which is what the readout then states.
                    self.playing = false
                    self.position = self.duration
                    return
                }
                self.position = player.currentTime
            }
        }
    }

    /// One stored recording, read chunk by chunk. The core never hands out a
    /// path, so the bytes come through the history row itself. A run that ends
    /// without a byte is a row with no recording, not an empty one to play.
    private func audio(for id: Int64) async throws -> Data {
        var data = Data()
        var offset = 0
        while true {
            let chunk: HistoryAudioChunk = try await core.request(
                "read_history_audio_chunk", HistoryAudioParams(historyId: id, offset: offset))
            if !chunk.bytes.isEmpty {
                data.append(contentsOf: chunk.bytes)
                offset += chunk.bytes.count
            }
            if chunk.eof { break }
            if chunk.bytes.isEmpty { throw LibraryFault.audioTruncated }
        }
        guard !data.isEmpty else { throw LibraryFault.audioMissing }
        return data
    }

    // MARK: - What is kept

    private func loadSettings() async {
        do {
            let settings: LibrarySettings = try await core.request("get_app_settings")
            limit = settings.historyLimit ?? 5
            retention = settings.recordingRetentionPeriod ?? .never
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func loadStorage() async {
        storage = try? await core.request("history_storage_status")
    }

    /// How many entries the core keeps. Zero keeps none, which is how the log
    /// is turned off.
    func updateLimit(_ value: Int) {
        let next = min(max(value, 0), Self.limitCeiling)
        guard next != limit else { return }
        savingRetention = true
        Task { [self] in
            do {
                try await core.request("update_history_limit", LibraryLimitParam(limit: next))
                limit = next
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
            savingRetention = false
        }
    }

    /// How long a recording survives after its words were written.
    func updateRetention(_ period: LibraryRetention) {
        guard period != retention else { return }
        savingRetention = true
        Task { [self] in
            do {
                try await core.request(
                    "update_recording_retention_period", LibraryRetentionParam(period: period.rawValue))
                retention = period
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
            savingRetention = false
        }
    }

    /// Runs one call, clears the last complaint when it works, and states the
    /// core's own words when it does not.
    private func call(_ work: @escaping @MainActor () async throws -> Void) {
        Task {
            do {
                try await work()
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

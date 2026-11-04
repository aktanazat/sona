import AppKit
import Foundation

/// Everything Home shows under the record button: this week's numbers, what
/// needs the reader, the latest conversations and notes, what Sona did on its
/// own, and the mode the next dictation runs in.
///
/// One store because the lists answer to the same handful of events: a
/// dictation lands, a meeting artifact changes, a meeting is removed, a mode
/// is switched. Splitting the reads would double the subscriptions for
/// nothing.
@MainActor
@Observable
final class OverviewStore {
    /// How far back the band can page: half a year of days, 26 weeks.
    private static let trendRange = "days_180"
    /// How far ahead the upcoming list looks.
    private static let upcomingDays = 7
    /// One page of the run log, and how many feed rows are worth drawing.
    private static let runPageSize = 20
    private static let receiptLimit = 3
    /// How many open loops the inbox is asked for. Home counts them, so a
    /// full answer reads as this many or more.
    static let openLoopLimit = 100
    private static let recentMeetingLimit = 20
    private static let recentNoteLimit = 8

    private(set) var trend: ActivityTrend?
    private(set) var meetings: ActivityMeetingTrend?
    /// All-time usage, under the paged week. Its own state rather than an
    /// optional: this region's whole content is the read, so a refusal gets
    /// a row and a retry where the trend's absence gets neither.
    private(set) var usage: UsageState = .loading
    /// Null until the suggestion read answers: an empty list and an unread
    /// list are not the same claim.
    private(set) var suggestions: [LearningEntry]?
    private(set) var receipts: FeedState<FeedReceipt> = .loading
    private(set) var openLoops: FeedState<FeedOpenLoop> = .loading
    /// The newest conversations and notes, each its own read so one failing
    /// leaves the other drawn.
    private(set) var recentMeetings: FeedState<MeetingHistorySummary> = .loading
    private(set) var recentNotes: FeedState<ScratchNote> = .loading
    private(set) var upcoming: OverviewUpcoming?
    /// The Today strip. Absent until the first answer, so it never draws
    /// counts it has not read.
    private(set) var today: HomeToday?
    private(set) var modes: CaptureModeSnapshot?
    private(set) var update: OverviewUpdate?
    /// Whether the update check has come back at all.
    private(set) var updateChecked = false
    private(set) var updateDismissed = false
    /// A dictation is in flight, so switching mode now would not touch it.
    private(set) var capturing = false
    private(set) var switchingMode = false
    /// The suggestions whose answer is still in flight, by row id.
    private(set) var answering: Set<String> = []
    /// The series whose write is in flight, so its row can quiet its
    /// controls. One at a time: the fence is one number for the whole pane,
    /// so two writes racing it would cost one of them a rejection.
    private(set) var savingSeries: String?
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?

    @ObservationIgnored private let core: Core
    /// A refresh that lands after a newer one started is stale.
    @ObservationIgnored private var feedGeneration = 0
    @ObservationIgnored private var recentGeneration = 0
    /// The sleep until the first upcoming event ends.
    @ObservationIgnored private var upcomingWake: Task<Void, Never>?
    /// The Today read waiting out a burst of meeting events.
    @ObservationIgnored private var todayRefresh: Task<Void, Never>?
    @ObservationIgnored private var todayGeneration = 0

    init(core: Core) {
        self.core = core

        core.observe(CoreEvent.historyUpdate) { [weak self] line in
            guard let self, let update: HistoryUpdate = try? Core.payload(line) else { return }
            // Saving a dictation changes no count on this page.
            if case .toggled = update { return }
            Task { await self.loadTrend() }
            Task { await self.loadUsage() }
            // Suggestions are mined from the dictations, so a new one may
            // have just earned a row.
            Task { await self.loadSuggestions() }
            self.refreshFeed()
        }
        // The startup unlock: a read refused while the database was locked
        // is asked again once storage says what it is.
        core.observe(CoreEvent.historyStorage) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadTrend() }
            Task { await self.loadUsage() }
        }
        core.observe(CoreEvent.overviewModesChanged) { [weak self] line in
            guard let self, let snapshot: CaptureModeSnapshot = try? Core.payload(line) else { return }
            self.modes = snapshot
        }
        // A meeting that finished, gained an artifact, or was removed changes
        // the recent lists and the meetings column alike; an answered
        // suggestion arrives as an artifact change with no session.
        core.observe(CoreEvent.overviewMeetingArtifactChanged) { [weak self] _ in
            guard let self else { return }
            self.refreshFeed()
            self.refreshToday()
            Task { await self.loadMeetingTrend() }
            Task { await self.loadSuggestions() }
            Task { await self.loadRecent() }
        }
        core.observe(CoreEvent.meetingSessionChanged) { [weak self] _ in
            guard let self else { return }
            self.refreshToday()
            Task { await self.loadMeetingTrend() }
            Task { await self.loadRecent() }
        }
        core.observe(CoreEvent.overviewMeetingRemoved) { [weak self] _ in
            guard let self else { return }
            self.refreshFeed()
            self.refreshToday()
            Task { await self.loadMeetingTrend() }
            Task { await self.loadRecent() }
        }
        core.observe(CoreEvent.scratchpadChanged) { [weak self] _ in
            Task { await self?.loadRecent() }
        }
        core.observe(CoreEvent.activity) { [weak self] line in
            guard let self, let activity: DictationActivity = try? Core.payload(line) else { return }
            self.capturing = activity.state != "idle"
        }
        // The calendar moves while this app is in the back: coming to the
        // front is the moment to read the week ahead again.
        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: nil
        ) { [weak self] _ in
            Task { await self?.loadUpcoming() }
            Task { await self?.refreshToday() }
        }
    }

    /// The first load. Every read goes out at once: none of them needs
    /// another's answer, and the page draws each list as it lands.
    func start() async {
        let reads = [
            Task { await self.loadTrend() },
            Task { await self.loadMeetingTrend() },
            Task { await self.loadUsage() },
            Task { await self.loadModes() },
            Task { await self.loadSuggestions() },
            Task { await self.loadUpcoming() },
            Task { await self.checkUpdate() },
            Task { await self.loadRecent() },
            Task { await self.loadToday() },
        ]
        refreshFeed()
        for read in reads {
            await read.value
        }
    }

    /// Answers one suggestion: accepted writes it into real settings, and the
    /// core replies with what is left pending.
    func answer(_ entry: LearningEntry, _ status: DecisionStatus) {
        guard !answering.contains(entry.id) else { return }
        answering.insert(entry.id)
        Task {
            await call {
                let request = DecisionRequest(
                    loopKind: entry.loopKind,
                    candidateKey: entry.candidateKey,
                    status: status,
                    displayText: entry.suggestion.displayText)
                let result: LearningResult = try await self.core.request("learning_decide", DecisionParams(request: request))
                self.suggestions = result.entries
            }
            answering.remove(entry.id)
        }
    }

    /// Switches the mode the next dictation runs in. The same write the
    /// mode-switch chord and the overlay's menu use, so there is one owner of
    /// which mode is current.
    func pick(_ mode: CaptureModeChoice) {
        guard mode.id != modes?.active?.id, !switchingMode else { return }
        switchingMode = true
        Task {
            do {
                let snapshot: CaptureModeSnapshot = try await core.request(
                    "set_active_mode", CaptureModeParams(modeId: mode.id))
                modes = snapshot
                error = nil
            } catch let failure {
                error = "Couldn't switch mode: \(failure.localizedDescription)"
            }
            switchingMode = false
        }
    }

    /// Closes the update row for this visit. Settings > About owns the
    /// standing answer and the button that asks again.
    func dismissUpdate() {
        updateDismissed = true
    }

    /// Runs one standing decision against the pane's fence.
    ///
    /// The commands and the consent receipt behind them belong to Meeting
    /// settings, so what arrives here is the call, not the wire: this holds
    /// the fence, the in-flight key, and what to do with the answer. A write
    /// that fails wrote nothing, and the row must stop showing the value that
    /// did not land, so the pane is read again before the failure is said.
    func writeSeries(_ seriesKey: String, _ send: @escaping @MainActor (Int) async throws -> OverviewSeriesWrite) {
        guard savingSeries == nil, let revision = upcoming?.seriesRevision else { return }
        savingSeries = seriesKey
        Task {
            do {
                let written = try await send(revision)
                upcoming?.patch(written)
                error = nil
            } catch let failure {
                await loadUpcoming()
                error = "Couldn't save that series setting: \(failure.localizedDescription)"
            }
            savingSeries = nil
        }
    }

    /// Both lists, on one refresh. Each keeps what it showed until its answer
    /// lands, so Home does not blink on every event; a read that fails keeps
    /// its own row.
    func refreshFeed() {
        feedGeneration += 1
        let generation = feedGeneration

        Task {
            do {
                let entries = try await loadReceipts()
                guard generation == feedGeneration else { return }
                receipts = .loaded(entries)
            } catch {
                guard generation == feedGeneration else { return }
                receipts = .failed
            }
        }
        Task {
            do {
                let result: FeedOpenLoops = try await core.request(
                    "open_loops_inbox", FeedLimitParams(limit: Self.openLoopLimit))
                guard generation == feedGeneration else { return }
                openLoops = .loaded(result.entries)
            } catch {
                guard generation == feedGeneration else { return }
                openLoops = .failed
            }
        }
    }

    /// One Today read for a burst of meeting events: a finishing meeting
    /// sends several in a row, and each would otherwise be its own round trip.
    func refreshToday() {
        todayRefresh?.cancel()
        todayRefresh = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await self?.loadToday()
        }
    }

    /// A refusal of the whole read is every count unreadable, never zero.
    private func loadToday() async {
        todayGeneration += 1
        let generation = todayGeneration
        let answer: HomeToday
        do {
            answer = try await core.request("home_today")
        } catch {
            answer = .unavailable
        }
        guard generation == todayGeneration else { return }
        today = answer
    }

    /// The recent conversations and notes. A list keeps what it showed until
    /// a newer answer replaces it, so a refresh never blanks the section.
    func loadRecent() async {
        recentGeneration += 1
        let generation = recentGeneration
        async let meetings = readRecentMeetings()
        async let notes = readRecentNotes()
        let (meetingState, noteState) = await (meetings, notes)
        guard generation == recentGeneration else { return }
        recentMeetings = meetingState
        recentNotes = noteState
    }

    private func readRecentMeetings() async -> FeedState<MeetingHistorySummary> {
        do {
            let page: MeetingsPage = try await core.request("meeting_list", ["limit": Self.recentMeetingLimit])
            return .loaded(page.entries)
        } catch {
            return .failed
        }
    }

    private func readRecentNotes() async -> FeedState<ScratchNote> {
        do {
            let notes: [ScratchNote] = try await core.request("scratchpad_recent", ["limit": Self.recentNoteLimit])
            return .loaded(notes)
        } catch {
            return .failed
        }
    }

    /// Pages the run log until three feed-worthy runs are found or the log
    /// ends. A quiet pass keeps its row in the full run log under Settings,
    /// not here.
    private func loadReceipts() async throws -> [FeedReceipt] {
        var found: [FeedReceipt] = []
        var cursor: FeedCursor?
        repeat {
            let request = FeedRunsRequest(cursor: cursor, limit: Self.runPageSize)
            let page: FeedRuns = try await core.request("workflow_runs", FeedRunsParams(request: request))
            for receipt in page.entries where receipt.belongsInFeed {
                found.append(receipt)
                if found.count == Self.receiptLimit {
                    return found
                }
            }
            cursor = page.nextCursor
        } while cursor != nil
        return found
    }

    /// The dictation column. The command fails with no message
    /// (`Result<_, ()>` in the core), and it fails on every launch until the
    /// keychain has unlocked the history database, so a refusal leaves the
    /// numbers absent, as the web overview did, and the unlock event below
    /// asks again.
    private func loadTrend() async {
        let request = ActivityTrendParams(request: ActivityTrendRequest(range: Self.trendRange))
        trend = try? await core.request("get_history_trend", request)
    }

    /// The meetings column. A refusal here is the same statement the trend's
    /// own `unavailable` case makes — no meeting storage to read — so it
    /// removes the column rather than putting a red line on the capture page.
    private func loadMeetingTrend() async {
        meetings = try? await core.request("meeting_trend",
                                           ActivityTrendParams(request: ActivityTrendRequest(range: Self.trendRange)))
    }

    /// The usage tiles. The read that follows a dictation replaces the
    /// numbers in place: the row never goes back to "Loading…" once it has
    /// shown a figure, and a refusal keeps its own row with a retry.
    private func loadUsage() async {
        do {
            let stats: UsageStats = try await core.request("get_history_usage_stats")
            usage = .loaded(stats, apps: stats.apps.map(Self.appRow))
        } catch {
            usage = .failed
        }
    }

    /// The retry on the failed row.
    func reloadUsage() {
        Task { await loadUsage() }
    }

    /// The identifier the row was saved with, as the app's name and icon on
    /// this Mac. An app no longer installed keeps its identifier: a guessed
    /// name would be a claim about the wrong app.
    private static func appRow(_ app: UsageApp) -> UsageAppRow {
        let workspace = NSWorkspace.shared
        guard let url = workspace.urlForApplication(withBundleIdentifier: app.applicationIdentifier) else {
            return UsageAppRow(
                id: app.applicationIdentifier, name: app.applicationIdentifier, icon: nil,
                dictations: app.dictations, words: app.words)
        }
        return UsageAppRow(
            id: app.applicationIdentifier,
            name: FileManager.default.displayName(atPath: url.path),
            icon: workspace.icon(forFile: url.path),
            dictations: app.dictations,
            words: app.words)
    }

    private func loadModes() async {
        await call {
            self.modes = try await self.core.request("get_modes")
        }
    }

    private func loadSuggestions() async {
        await call {
            let result: LearningResult = try await self.core.request("learning_suggestions")
            self.suggestions = result.entries
        }
    }

    /// The week ahead, then a wake at the moment its first row ends, so an
    /// event that has passed leaves the list without a visit to prompt it.
    private func loadUpcoming() async {
        await call {
            self.upcoming = try await self.core.request(
                "meeting_upcoming_events", OverviewUpcomingParams(days: Self.upcomingDays))
        }
        upcomingWake?.cancel()
        guard let next = upcoming?.rows.map(\.endUtcMs).min() else { return }
        let wait = Double(next) / 1000 - Date.now.timeIntervalSince1970
        upcomingWake = Task { [weak self] in
            try? await Task.sleep(for: .seconds(max(wait, 1)))
            guard !Task.isCancelled else { return }
            await self?.loadUpcoming()
        }
    }

    /// One check per visit. A check that does not come back says nothing
    /// about the app in front of you, so nothing is drawn for it and the page
    /// keeps no error for it either.
    private func checkUpdate() async {
        update = try? await core.request("check_for_updates")
        updateChecked = true
    }

    private func call(_ work: @MainActor () async throws -> Void) async {
        do {
            try await work()
            error = nil
        } catch let failure {
            error = failure.localizedDescription
        }
    }
}

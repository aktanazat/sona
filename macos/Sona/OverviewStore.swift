import Foundation

/// Everything the capture page shows under the record button: this week's
/// numbers, what needs the reader, what Sona did on its own, and the mode the
/// next dictation runs in.
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
    /// How many open loops the inbox is asked for.
    private static let openLoopLimit = 5

    private(set) var trend: ActivityTrend?
    private(set) var meetings: ActivityMeetingTrend?
    /// All-time dictation totals, under the paged week.
    private(set) var stats: HistoryStats?
    /// Null until the suggestion read answers: an empty list and an unread
    /// list are not the same claim.
    private(set) var suggestions: [LearningEntry]?
    private(set) var receipts: FeedState<FeedReceipt> = .loading
    private(set) var openLoops: FeedState<FeedOpenLoop> = .loading
    private(set) var upcoming: OverviewUpcoming?
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

    init(core: Core) {
        self.core = core

        core.observe(CoreEvent.historyUpdate) { [weak self] line in
            guard let self, let update: HistoryUpdate = try? Core.payload(line) else { return }
            // Saving a dictation changes no count on this page.
            if case .toggled = update { return }
            Task { await self.loadTrend() }
            Task { await self.loadStats() }
            self.refreshFeed()
        }
        // The startup unlock: a read refused while the database was locked
        // is asked again once storage says what it is.
        core.observe(CoreEvent.historyStorage) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadTrend() }
            Task { await self.loadStats() }
        }
        core.observe(CoreEvent.overviewModesChanged) { [weak self] line in
            guard let self, let snapshot: CaptureModeSnapshot = try? Core.payload(line) else { return }
            self.modes = snapshot
        }
        core.observe(CoreEvent.overviewMeetingArtifactChanged) { [weak self] _ in
            self?.refreshFeed()
        }
        core.observe(CoreEvent.overviewMeetingRemoved) { [weak self] _ in
            self?.refreshFeed()
        }
        core.observe(CoreEvent.activity) { [weak self] line in
            guard let self, let activity: DictationActivity = try? Core.payload(line) else { return }
            self.capturing = activity.state != "idle"
        }
    }

    /// The first load. Every read goes out at once: none of them needs
    /// another's answer, and the page draws each list as it lands.
    func start() async {
        let reads = [
            Task { await self.loadTrend() },
            Task { await self.loadMeetingTrend() },
            Task { await self.loadStats() },
            Task { await self.loadModes() },
            Task { await self.loadSuggestions() },
            Task { await self.loadUpcoming() },
            Task { await self.checkUpdate() },
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

    /// Both lists, on one refresh. A read that fails keeps its own row.
    func refreshFeed() {
        feedGeneration += 1
        let generation = feedGeneration
        receipts = .loading
        openLoops = .loading

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

    /// The dictation column and the all-time line. Both commands fail with no
    /// message (`Result<_, ()>` in the core), and they fail on every launch
    /// until the keychain has unlocked the history database, so a refusal
    /// leaves the numbers absent, as the web overview did, and the unlock
    /// event below asks again.
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

    private func loadStats() async {
        stats = try? await core.request("get_history_stats")
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

    private func loadUpcoming() async {
        await call {
            self.upcoming = try await self.core.request(
                "meeting_upcoming_events", OverviewUpcomingParams(days: Self.upcomingDays))
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

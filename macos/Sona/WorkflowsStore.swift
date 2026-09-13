import Foundation

/// What Sona does on its own after a meeting, and what it did.
///
/// Shapes mirror `src-tauri/src/meeting/workflow_types.rs`; the copy mirrors
/// `src/components/settings/workflows/`. The switches say what a workflow
/// does in plain language, because "Person linking" names a subsystem and
/// "Remember people" names the outcome.

extension CoreEvent {
    /// A pass rewrote a meeting's artifacts: the run log has a new row.
    static let workflowArtifactChanged = "meeting:artifact-changed"
    static let workflowTranscriptChanged = "meeting:transcript-changed"
    static let workflowSessionChanged = "meeting:session-changed"
}

/// One workflow's id, kept as the core spells it so an id this build has
/// never heard of still renders as itself rather than breaking the page.
struct WorkflowId: Hashable, Codable, Identifiable {
    let rawValue: String

    var id: String { rawValue }

    init(_ rawValue: String) {
        self.rawValue = rawValue
    }

    init(from decoder: Decoder) throws {
        rawValue = try decoder.singleValueContainer().decode(String.self)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    /// What this workflow is called where a person reads it.
    var name: String {
        switch rawValue {
        case "person_linking": "Remember people"
        case "pre_meeting_briefing": "Prepare meeting briefs"
        case "continuity": "Carry meetings forward"
        case "vocabulary_mining": "Learn new words"
        case "document_linking": "Link documents"
        case "meeting_activity": "Meeting recording"
        case "spoken_punctuation": "Suggest spoken punctuation"
        case "correction_learning": "Learn from your corrections"
        case "mode_habits": "Suggest mode rules"
        case "capture_advisor": "Advise on recording quality"
        case "series_priming": "Prime recurring meetings"
        case "daily_digest": "Evening digest"
        default: rawValue
        }
    }

    /// What it does, for the ones the Settings list can switch off. The three
    /// permanent workflows have a name and no description.
    var detail: String? {
        switch rawValue {
        case "person_linking":
            "Connects meetings to people using confirmed attendee and speaker evidence."
        case "pre_meeting_briefing":
            "Builds a brief from past meetings before a detected meeting starts."
        case "continuity":
            "Carries unresolved commitments forward across meetings."
        case "vocabulary_mining":
            "Finds repeated names and terms that may belong in your vocabulary."
        case "document_linking":
            "Connects imported documents to people named in them."
        case "spoken_punctuation":
            "Notices symbols you say out loud that no replacement rule writes yet."
        case "correction_learning":
            "Turns fixes you make by hand into vocabulary suggestions."
        case "mode_habits":
            "Notices a mode you keep reaching for by shortcut."
        case "capture_advisor":
            "Reports retries, lost recordings, and quiet audio from your own dictations."
        default:
            nil
        }
    }
}

/// How one run ended.
enum WorkflowRunStatus: String, Decodable {
    case ok
    case failed
    case skipped
    /// A status this build does not know: drawn like a skip, never hidden.
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = WorkflowRunStatus(rawValue: raw) ?? .unknown
    }

    var label: String {
        switch self {
        case .ok: "Completed"
        case .failed: "Failed"
        case .skipped, .unknown: "Skipped"
        }
    }
}

/// What one run counted. Every field is a count the outcome sentence reads.
struct WorkflowOutcomeCounts: Decodable {
    let changes: Int
    let persons: Int
    let series: Int
    let carried: Int
    let candidates: Int
    let suggestions: Int
    let terms: Int
    let meetings: Int
    let loopsClosed: Int
    let suggestionsWaiting: Int
    let waitingOnStale: Int
}

/// The receipt one run wrote. This *is* the record: nothing retries, and a
/// failed row stays visible.
struct WorkflowRunReceipt: Decodable, Identifiable {
    let id: String
    let workflowId: WorkflowId
    /// The meeting or document the run was about, when there is one to open.
    let jumpTarget: FeedJump?
    let status: WorkflowRunStatus
    let startedAtUtcMs: Double
    let finishedAtUtcMs: Double
    /// The core's own sentence, used when this build does not know the code.
    let outcomeSummary: String
    let outcomeCode: String
    let outcomeCounts: WorkflowOutcomeCounts
    let error: String?

    /// Whether this run comes after `other` in the log's order: newest
    /// first, the id breaking a tie the way the core's cursor does.
    func isOlder(than other: WorkflowRunReceipt) -> Bool {
        startedAtUtcMs < other.startedAtUtcMs
            || (startedAtUtcMs == other.startedAtUtcMs && id < other.id)
    }

    /// The stored code, said for a person: what went wrong, whether it comes
    /// back on its own, and what to do if it does not. The code stays as
    /// the detail, since it is what a bug report needs. Nothing retries a
    /// run; the next meeting that qualifies starts a new one.
    var failureText: String? {
        guard let error else { return nil }
        let sentence: String
        switch error {
        case "storage_unavailable", "store_unavailable", "io_error":
            sentence = "Sona's meeting storage couldn't be reached. Nothing was changed; the next meeting runs it again."
        case "encryption_unavailable":
            sentence = "Sona's meeting storage was locked. Nothing was changed; the next meeting runs it again."
        case "store_corrupt":
            sentence = "A stored record couldn't be read. Nothing was changed."
        case "invalid_event_payload":
            sentence = "The meeting record this run was given was incomplete. Nothing was changed."
        case "workflow_panicked":
            sentence = "This run hit a bug and stopped before writing anything. Nothing was changed."
        case "local_model_unavailable":
            sentence = "The local model this run needs isn't available. Nothing was changed; check Models."
        default:
            return error
        }
        return "\(sentence) (\(error))"
    }
}

/// One workflow in the Settings list.
struct WorkflowSummary: Decodable, Identifiable {
    let id: WorkflowId
    let enabled: Bool
    let lastRun: WorkflowRunReceipt?
}

struct WorkflowsListResult: Decodable {
    let revision: UInt64
    let entries: [WorkflowSummary]
}

/// Where the next page of runs resumes.
struct WorkflowRunCursor: Codable {
    let startedAtUtcMs: Double
    let runId: String

    private enum DecodeKey: String, CodingKey {
        case startedAtUtcMs
        case runId
    }

    private enum EncodeKey: String, CodingKey {
        case startedAtUtcMs = "started_at_utc_ms"
        case runId = "run_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: DecodeKey.self)
        startedAtUtcMs = try container.decode(Double.self, forKey: .startedAtUtcMs)
        runId = try container.decode(String.self, forKey: .runId)
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: EncodeKey.self)
        try container.encode(startedAtUtcMs, forKey: .startedAtUtcMs)
        try container.encode(runId, forKey: .runId)
    }
}

struct WorkflowRunsPage: Decodable {
    let entries: [WorkflowRunReceipt]
    let nextCursor: WorkflowRunCursor?
}

/// Runs per local calendar day over the last seven days, today last, over
/// every run the core holds: the number is the week's, not the page's.
struct WorkflowRunTrend: Decodable {
    struct Point: Decodable {
        let localDate: String
        let runs: Int
    }

    let total: Int
    let points: [Point]
}

/// What a run did, said the way a person would say it. One sentence per
/// outcome code, counted from the receipt the run wrote — never from the
/// workflow's name, which is the subsystem's word for itself.
func workflowOutcomeText(_ receipt: WorkflowRunReceipt) -> String {
    let counts = receipt.outcomeCounts
    switch receipt.outcomeCode {
    case "person_links":
        return counts.changes == 1
            ? "Remembered 1 person"
            : "Remembered \(counts.changes) people"
    case "briefing": return "Prepared your meeting brief"
    case "continuity":
        return counts.carried == 1
            ? "Carried 1 open loop forward"
            : "Carried \(counts.carried) open loops forward"
    /* The pass finds candidates for the vocabulary; nothing is learned
     * until one is accepted, so the sentence says what was found. */
    case "vocabulary_candidates":
        switch counts.candidates {
        case 0: return "Found no new words"
        case 1: return "Found 1 word for your vocabulary"
        default: return "Found \(counts.candidates) words for your vocabulary"
        }
    /* The run is about one imported document; what it counts is the
     * people it connected the document to. */
    case "document_links":
        switch counts.persons {
        case 0: return "Found no one to link this document to"
        case 1: return "Linked this document to 1 person"
        default: return "Linked this document to \(counts.persons) people"
        }
    case "learning_suggestions":
        return counts.suggestions == 1 ? "Noticed 1 thing" : "Noticed \(counts.suggestions) things"
    case "series_primed": return "Prepared a recurring meeting"
    /* D20 narrates a day, not a change: the run writes nothing, so its
     * receipt existing is the whole sentence. */
    case "digest_raised": return "Summed up the day"
    /* The consent popup's own history, narrated: what happened to the
     * recording, never what happened to a prompt or a receipt. */
    case "prompt_recorded": return "Started recording"
    case "prompt_ignored": return "Skipped recording a detected meeting"
    case "auto_record_started": return "Started recording automatically"
    case "auto_record_stopped": return "Stopped recording automatically"
    case "prep_presented": return "Prep"
    case "prep_record_armed": return "Record when it starts"
    case "prep_brief_opened": return "Open brief"
    case "prep_dismissed": return "Dismiss"
    case "wrap_presented": return "Wrap"
    case "wrap_notes_opened": return "Open notes"
    case "wrap_follow_up_copied": return "Copied"
    case "wrap_done": return "Done"
    case "already_processed": return "Nothing new to do"
    case "failed": return "Couldn't finish"
    case "skipped": return "Skipped"
    default: return receipt.outcomeSummary
    }
}

/// The workflow catalogue and its run log.
@MainActor
@Observable
final class WorkflowsStore {
    /// Which read of the log failed, so the retry repeats that one.
    enum RunLoad {
        case firstPage
        case nextPage
    }

    private(set) var entries: [WorkflowSummary] = []
    private(set) var revision: UInt64 = 0
    private(set) var receipts: [WorkflowRunReceipt] = []
    private(set) var trend: WorkflowRunTrend?
    private(set) var loadingWorkflows = true
    private(set) var loadingRuns = true
    private(set) var loadingMore = false
    /// The switch waiting on the core; every switch is quiet while one is.
    private(set) var pending: WorkflowId?
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    /// The run log's own failure, and which read it was.
    private(set) var runError: (load: RunLoad, message: String)?

    @ObservationIgnored private var nextCursor: WorkflowRunCursor?
    /// Bumped by every read of the first page. A page of older runs that
    /// comes back after the log moved on is dropped, not appended.
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private let core: Core

    var hasMoreRuns: Bool { nextCursor != nil }

    init(core: Core) {
        self.core = core
        for event in [
            CoreEvent.workflowArtifactChanged,
            CoreEvent.workflowTranscriptChanged,
            CoreEvent.workflowSessionChanged,
            CoreEvent.historyUpdate,
        ] {
            core.observe(event) { [weak self] _ in
                Task { await self?.reload() }
            }
        }
    }

    func start() async {
        await reload()
    }

    /// Every half at once: the switches, the newest runs, the week.
    func reload() async {
        async let list: Void = loadWorkflows()
        async let runs: Void = loadFirstRunPage()
        async let week: Void = loadTrend()
        _ = await (list, runs, week)
    }

    func loadWorkflows() async {
        loadingWorkflows = true
        defer { loadingWorkflows = false }
        do {
            let result: WorkflowsListResult = try await core.request("workflows_list")
            apply(result)
            error = nil
        } catch {
            self.error = "Couldn't load workflows. \(promptErrorSentence(error))"
        }
    }

    /// The newest page. Rows already read past it stay: a meeting ending
    /// while someone is reading last month's runs must not scroll them back
    /// to the top. Only a page that no longer reaches the rows on screen,
    /// more than a page of new runs, starts the log over.
    func loadFirstRunPage() async {
        generation += 1
        loadingRuns = true
        defer { loadingRuns = false }
        do {
            let page: WorkflowRunsPage = try await core.request(
                "workflow_runs", ["request": RunsRequest(cursor: nil)])
            merge(page)
            runError = nil
        } catch {
            runError = (.firstPage, "Couldn't load activity. \(promptErrorSentence(error))")
        }
    }

    /// The next page, appended. The cursor is the core's, handed back.
    func loadMoreRuns() async {
        guard let cursor = nextCursor, !loadingMore else { return }
        let generation = generation
        loadingMore = true
        defer { loadingMore = false }
        do {
            let page: WorkflowRunsPage = try await core.request(
                "workflow_runs", ["request": RunsRequest(cursor: cursor)])
            guard generation == self.generation else { return }
            receipts += page.entries
            nextCursor = page.nextCursor
            runError = nil
        } catch {
            guard generation == self.generation else { return }
            runError = (.nextPage, "Couldn't load more activity. \(promptErrorSentence(error))")
        }
    }

    /// Repeats the read that failed.
    func retryRuns() async {
        switch runError?.load {
        case .firstPage: await loadFirstRunPage()
        case .nextPage: await loadMoreRuns()
        case nil: break
        }
    }

    func loadTrend() async {
        do {
            trend = try await core.request("workflow_run_trend", ["request": ["range": "days_7"]])
        } catch {
            trend = nil
        }
    }

    private func merge(_ page: WorkflowRunsPage) {
        guard let head = receipts.first, let last = page.entries.last,
              page.entries.contains(where: { $0.id == head.id }) else {
            receipts = page.entries
            nextCursor = page.nextCursor
            return
        }
        let older = receipts.filter { $0.isOlder(than: last) }
        receipts = page.entries + older
        if older.isEmpty {
            nextCursor = page.nextCursor
        }
    }

    /// Turns one workflow on or off, carrying the revision the list was read
    /// at. The answer is the whole list again.
    func setEnabled(_ workflowId: WorkflowId, _ enabled: Bool) async {
        guard pending == nil, !entries.isEmpty else { return }
        pending = workflowId
        error = nil
        defer { pending = nil }
        do {
            let result: WorkflowsListResult = try await core.request(
                "workflow_set_enabled",
                [
                    "request": SetEnabledRequest(
                        workflowId: workflowId, enabled: enabled, expectedRevision: revision)
                ])
            apply(result)
        } catch {
            self.error = "Couldn't update the workflow. \(promptErrorSentence(error))"
        }
    }

    private struct RunsRequest: Encodable {
        let cursor: WorkflowRunCursor?
        /// How many runs one page holds.
        let limit = 50
        /// Every workflow, the way the Settings run log reads it.
        let workflowId: WorkflowId? = nil

        enum CodingKeys: String, CodingKey {
            case cursor
            case limit
            case workflowId = "workflow_id"
        }
    }

    private struct SetEnabledRequest: Encodable {
        let workflowId: WorkflowId
        let enabled: Bool
        let expectedRevision: UInt64

        enum CodingKeys: String, CodingKey {
            case workflowId = "workflow_id"
            case enabled
            case expectedRevision = "expected_revision"
        }
    }

    private func apply(_ result: WorkflowsListResult) {
        entries = result.entries
        revision = result.revision
    }
}

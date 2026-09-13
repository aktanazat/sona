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
    let status: WorkflowRunStatus
    let startedAtUtcMs: Double
    let finishedAtUtcMs: Double
    /// The core's own sentence, used when this build does not know the code.
    let outcomeSummary: String
    let outcomeCode: String
    let outcomeCounts: WorkflowOutcomeCounts
    let error: String?
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
    case "vocabulary_candidates":
        return counts.candidates == 1 ? "Learned a new word" : "Learned \(counts.candidates) new words"
    case "document_links":
        return counts.changes == 1 ? "Linked a document" : "Linked \(counts.changes) documents"
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

/// Counts the loaded receipts into the seven local calendar days ending today.
func workflowRunsPerDay(_ receipts: [WorkflowRunReceipt], now: Date = .now) -> [Int] {
    let calendar = Calendar.current
    let today = calendar.startOfDay(for: now)
    let days = (0..<7).compactMap { offset in
        calendar.date(byAdding: .day, value: offset - 6, to: today)
    }
    var values = [Int](repeating: 0, count: days.count)
    for receipt in receipts {
        let day = calendar.startOfDay(for: Date(timeIntervalSince1970: receipt.startedAtUtcMs / 1000))
        if let index = days.firstIndex(of: day) {
            values[index] += 1
        }
    }
    return values
}

/// The workflow catalogue and its run log.
@MainActor
@Observable
final class WorkflowsStore {
    private(set) var entries: [WorkflowSummary] = []
    private(set) var revision: UInt64 = 0
    private(set) var receipts: [WorkflowRunReceipt] = []
    private(set) var loadingWorkflows = true
    private(set) var loadingRuns = true
    private(set) var loadingMore = false
    /// The switch waiting on the core; every switch is quiet while one is.
    private(set) var pending: WorkflowId?
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    /// The run log's own failure: it retries on its own button.
    private(set) var runError: String?

    @ObservationIgnored private var nextCursor: WorkflowRunCursor?
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

    /// Both halves, the way the React hook loads them: together.
    func reload() async {
        async let list: Void = loadWorkflows()
        async let runs: Void = loadFirstRunPage()
        _ = await (list, runs)
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

    func loadFirstRunPage() async {
        loadingRuns = true
        defer { loadingRuns = false }
        do {
            let page: WorkflowRunsPage = try await core.request(
                "workflow_runs", ["request": RunsRequest(cursor: nil)])
            receipts = page.entries
            nextCursor = page.nextCursor
            runError = nil
        } catch {
            runError = "Couldn't load activity. \(promptErrorSentence(error))"
        }
    }

    /// The next page, appended. The cursor is the core's, handed back.
    func loadMoreRuns() async {
        guard let cursor = nextCursor, !loadingMore else { return }
        loadingMore = true
        defer { loadingMore = false }
        do {
            let page: WorkflowRunsPage = try await core.request(
                "workflow_runs", ["request": RunsRequest(cursor: cursor)])
            receipts += page.entries
            nextCursor = page.nextCursor
            runError = nil
        } catch {
            runError = "Couldn't load more activity. \(promptErrorSentence(error))"
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

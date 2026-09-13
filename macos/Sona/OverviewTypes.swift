import Foundation

/// The wire shapes the Overview page reads, and the sentences it writes from
/// them. Field names mirror the Rust structs with snake_case turned into
/// camelCase by `Core.decoder`; request structs spell their keys out, because
/// params are encoded with a plain `JSONEncoder` and serde expects the Rust
/// field names.

extension CoreEvent {
    /// `ModeSettingsSnapshot`, every time a mode is added, edited or switched.
    static let overviewModesChanged = "modes-changed-event"
    /// A meeting grew or lost an artifact: the recent list may have a new row.
    static let overviewMeetingArtifactChanged = "meeting:artifact-changed"
    /// A meeting was deleted, so a row in either list may name nothing.
    static let overviewMeetingRemoved = "meeting:removed"
}

// MARK: - Activity

/// One local-calendar day of dictation. Every day in the range is present,
/// including the ones with nothing on them.
struct ActivityTrendPoint: Decodable {
    let localDate: String
    let recordings: Int
    let durationMs: Int64
    let words: Int
}

struct ActivityTrendTotals: Decodable {
    let recordings: Int
    let durationMs: Int64
    let words: Int
}

/// `get_history_trend`: a bounded projection over retained dictation history.
struct ActivityTrend: Decodable {
    let rangeStartLocalDate: String
    let rangeEndLocalDate: String
    let allTime: ActivityTrendTotals
    let rangeTotal: ActivityTrendTotals
    let activeDays: Int
    let currentStreakDays: Int
    let points: [ActivityTrendPoint]
}

/// One local-calendar day of meetings.
struct ActivityMeetingPoint: Decodable {
    let localDate: String
    let meetings: Int
    let verifiedCapturedDurationMs: Int64
}

/// `meeting_trend`. Storage that cannot answer has no zero-valued projection,
/// so "unavailable" is its own case and never a week of empty bars.
enum ActivityMeetingTrend: Decodable {
    case available([ActivityMeetingPoint])
    case unavailable

    private enum Key: String, CodingKey { case status, points }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let status = try container.decode(String.self, forKey: .status)
        switch status {
        case "available": self = .available(try container.decode([ActivityMeetingPoint].self, forKey: .points))
        case "unavailable": self = .unavailable
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .status, in: container, debugDescription: "unknown meeting trend status \(status)")
        }
    }
}

/// The range both trend commands take. Half a year, so the band can page back
/// through 26 weeks without asking again.
struct ActivityTrendRequest: Encodable {
    let range: String
}

struct ActivityTrendParams: Encodable {
    let request: ActivityTrendRequest
}

/// Seven days ending `page` weeks before today, out of a trend's points.
/// Page 0 is the current week; a page past the oldest data clamps to it.
func activityWindow(_ points: [ActivityTrendPoint], page: Int) -> (page: Int, start: Int, points: ArraySlice<ActivityTrendPoint>) {
    let pageCount = max(1, Int((Double(points.count) / Double(ActivityChart.slots)).rounded(.up)))
    let page = min(max(0, page), pageCount - 1)
    let end = points.count - page * ActivityChart.slots
    let start = max(0, end - ActivityChart.slots)
    guard start < end else { return (page, 0, []) }
    return (page, start, points[start..<end])
}

// MARK: - Learning

/// Which loop mined a candidate. The two vocabulary kinds are separate
/// identities: a term is a phrase Sona keeps hearing, a correction is a
/// rewrite a human performed.
enum LearningLoop: String, Codable {
    case spokenPunctuation = "spoken_punctuation"
    case vocabularyTerm = "vocabulary_term"
    case vocabularyCorrection = "vocabulary_correction"
    case modeHabit = "mode_habit"
    case captureAdvice = "capture_advice"
}

/// Which capture statistic loop 6 is reporting.
enum LearningAdvice: String, Decodable {
    case retryRate = "retry_rate"
    case lostCaptureRate = "lost_capture_rate"
    case inputLevel = "input_level"
}

/// One suggestion's content, rebuilt on read from stored evidence.
enum LearningSuggestion: Decodable {
    case spokenPunctuation(spoken: String, written: String)
    case vocabularyCorrection(spoken: String, written: String)
    case modeHabit(modeId: String, modeName: String)
    case captureAdvice(advice: LearningAdvice, subject: String, statPermille: Int)

    private enum Key: String, CodingKey {
        case kind, spoken, written, modeId, modeName, advice, subject, statPermille
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "spoken_punctuation":
            self = .spokenPunctuation(
                spoken: try container.decode(String.self, forKey: .spoken),
                written: try container.decode(String.self, forKey: .written))
        case "vocabulary_correction":
            self = .vocabularyCorrection(
                spoken: try container.decode(String.self, forKey: .spoken),
                written: try container.decode(String.self, forKey: .written))
        case "mode_habit":
            self = .modeHabit(
                modeId: try container.decode(String.self, forKey: .modeId),
                modeName: try container.decode(String.self, forKey: .modeName))
        case "capture_advice":
            self = .captureAdvice(
                advice: try container.decode(LearningAdvice.self, forKey: .advice),
                subject: try container.decode(String.self, forKey: .subject),
                statPermille: try container.decode(Int.self, forKey: .statPermille))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown suggestion kind \(kind)")
        }
    }

    /// The question, as a person would ask it.
    var headline: String {
        switch self {
        case let .spokenPunctuation(spoken, written):
            "You say \(spoken) a lot. Write it as \(written)?"
        case let .vocabularyCorrection(spoken, written):
            "You keep changing \(spoken) to \(written). Learn it?"
        case let .modeHabit(_, modeName):
            "You reach for \(modeName) by shortcut most days. Make it your default?"
        case let .captureAdvice(advice, subject, statPermille):
            switch advice {
            case .retryRate:
                "Dictations on \(subject) get retried \(LearningSuggestion.times(statPermille))× more often than the rest."
            case .lostCaptureRate:
                "Dictations on \(subject) end cut off or silent \(LearningSuggestion.times(statPermille))× more often than the rest."
            case .inputLevel:
                "\(Int((Double(statPermille) / 10).rounded()))% of your recordings came in quiet. Check your microphone level?"
            }
        }
    }

    /// What the core remembers as the answer's subject. Loop 4 primes a
    /// session's recognizer from accepted vocabulary lines, so those stay the
    /// bare term the reader agreed to and never a rendered sentence.
    var displayText: String {
        switch self {
        case let .spokenPunctuation(spoken, _): spoken
        case let .vocabularyCorrection(spoken, _): spoken
        case let .modeHabit(_, modeName): modeName
        case .captureAdvice: headline
        }
    }

    /// Advice is an observation: there is nothing to accept, only something to
    /// stop being told.
    var acceptable: Bool {
        if case .captureAdvice = self { false } else { true }
    }

    private static func times(_ permille: Int) -> String {
        String(format: "%.1f", Double(permille) / 1000)
    }
}

/// Why a suggestion exists, counted from the corpus that produced it.
struct LearningEvidence: Decodable {
    let occurrences: Int
    let distinctDays: Int

    /// "12 times, across 5 days": the reason the question is being asked.
    var sentence: String {
        let times = occurrences == 1 ? "1 time" : "\(occurrences) times"
        let days = distinctDays == 1 ? "1 day" : "\(distinctDays) days"
        return "\(times), across \(days)"
    }
}

/// One pending suggestion as the feed reads it.
struct LearningEntry: Decodable, Identifiable {
    let loopKind: LearningLoop
    let candidateKey: String
    let suggestion: LearningSuggestion
    let evidence: LearningEvidence

    /// The candidate identity the core keyed it by, which is also what makes
    /// two rows of the same wording distinct.
    var id: String { "\(loopKind.rawValue):\(candidateKey)" }
}

struct LearningResult: Decodable {
    let revision: Int
    let entries: [LearningEntry]
}

// MARK: - Decisions

/// What a human answered. There is no third state: a candidate is pending
/// because no row exists for it.
enum DecisionStatus: String, Encodable {
    case accepted
    case dismissed
}

/// One answer, sent back on the candidate key the miner generated.
struct DecisionRequest: Encodable {
    let loopKind: LearningLoop
    let candidateKey: String
    let status: DecisionStatus
    let displayText: String

    private enum WireKey: String, CodingKey {
        case loopKind = "loop_kind"
        case candidateKey = "candidate_key"
        case status
        case displayText = "display_text"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(loopKind, forKey: .loopKind)
        try container.encode(candidateKey, forKey: .candidateKey)
        try container.encode(status, forKey: .status)
        try container.encode(displayText, forKey: .displayText)
    }
}

struct DecisionParams: Encodable {
    let request: DecisionRequest
}

// MARK: - Capture mode

/// One mode, as much of it as the chip shows. The Modes screen owns the rest.
struct CaptureModeChoice: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
}

/// `get_modes` and `set_active_mode` both answer with this.
struct CaptureModeSnapshot: Decodable {
    let modes: [CaptureModeChoice]
    let activeModeId: String
    let revision: Int

    /// The mode the next dictation runs in. A snapshot whose active id names
    /// nothing falls back to the first mode, the way the core's own resolver
    /// does, rather than drawing a chip for a mode this install lacks.
    var active: CaptureModeChoice? {
        modes.first { $0.id == activeModeId } ?? modes.first
    }
}

struct CaptureModeParams: Encodable {
    let modeId: String
}

// MARK: - Feed

/// What a list is: still reading, read, or refused. An empty list and an
/// unread list are not the same claim.
enum FeedState<Item> {
    case loading
    case loaded([Item])
    case failed
}

/// Where a receipt's row goes when it is opened.
enum FeedJump: Decodable {
    case meeting(String)
    case document(String)

    private enum Key: String, CodingKey { case kind, sessionId, documentId }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "meeting": self = .meeting(try container.decode(String.self, forKey: .sessionId))
        case "document": self = .document(try container.decode(String.self, forKey: .documentId))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown jump target \(kind)")
        }
    }

    var meetingId: String? {
        if case let .meeting(id) = self { id } else { nil }
    }
}

/// What one run counted. Only the fields a sentence or the effect test reads.
struct FeedCounts: Decodable {
    let changes: Int
    let carried: Int
    let candidates: Int
    let suggestions: Int
}

/// One finished workflow run.
struct FeedReceipt: Decodable, Identifiable {
    let id: String
    let workflowId: String
    let jumpTarget: FeedJump?
    let status: String
    let finishedAtUtcMs: Int64
    let outcomeSummary: String
    let outcomeCode: String
    let outcomeCounts: FeedCounts

    /// What the run did, said the way a person would say it. A code this shell
    /// does not know yet falls back to the core's own summary rather than to
    /// the workflow's name, which is the subsystem's word for itself.
    var sentence: String {
        switch outcomeCode {
        case "person_links":
            outcomeCounts.changes == 1 ? "Remembered 1 person" : "Remembered \(outcomeCounts.changes) people"
        case "briefing": "Prepared your meeting brief"
        case "continuity":
            outcomeCounts.carried == 1 ? "Carried 1 open loop forward" : "Carried \(outcomeCounts.carried) open loops forward"
        case "vocabulary_candidates":
            outcomeCounts.candidates == 1 ? "Learned a new word" : "Learned \(outcomeCounts.candidates) new words"
        case "document_links":
            outcomeCounts.changes == 1 ? "Linked a document" : "Linked \(outcomeCounts.changes) documents"
        case "learning_suggestions":
            outcomeCounts.suggestions == 1 ? "Noticed 1 thing" : "Noticed \(outcomeCounts.suggestions) things"
        case "series_primed": "Prepared a recurring meeting"
        case "digest_raised": "Summed up the day"
        case "prompt_recorded": "Started recording"
        case "prompt_ignored": "Skipped recording a detected meeting"
        case "auto_record_started": "Started recording automatically"
        case "auto_record_stopped": "Stopped recording automatically"
        case "prep_presented": "Prep"
        case "prep_record_armed": "Record when it starts"
        case "prep_brief_opened": "Open brief"
        case "prep_dismissed": "Dismiss"
        case "wrap_presented": "Wrap"
        case "wrap_notes_opened": "Open notes"
        case "wrap_follow_up_copied": "Copied"
        case "wrap_done": "Done"
        case "already_processed": "Nothing new to do"
        case "failed": "Couldn't finish"
        case "skipped": "Skipped"
        default: outcomeSummary
        }
    }

    /// Whether that sentence names something that happened to the reader's
    /// data. A pass that found nothing still writes a receipt, and the run log
    /// under Settings is where those belong.
    var hasEffect: Bool {
        switch outcomeCode {
        case "person_links", "document_links": outcomeCounts.changes > 0
        case "continuity": outcomeCounts.carried > 0
        case "vocabulary_candidates": outcomeCounts.candidates > 0
        case "learning_suggestions": outcomeCounts.suggestions > 0
        case "already_processed", "skipped": false
        default: true
        }
    }

    /// The workflow's human name, for the row's meta line.
    var source: String {
        switch workflowId {
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
        default: workflowId
        }
    }

    /// What the recent list will show: a run that succeeded, changed
    /// something, and that the reader can act on. Skipping a detected meeting
    /// is the exception that leaves no session to open and is still worth a
    /// line.
    var belongsInFeed: Bool {
        status == "ok" && hasEffect && (jumpTarget?.meetingId != nil || workflowId == "meeting_activity")
    }
}

/// Where the next page of runs starts.
struct FeedCursor: Codable {
    let startedAtUtcMs: Int64
    let runId: String

    private enum WireKey: String, CodingKey {
        case startedAtUtcMs = "started_at_utc_ms"
        case runId = "run_id"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(startedAtUtcMs, forKey: .startedAtUtcMs)
        try container.encode(runId, forKey: .runId)
    }
}

struct FeedRuns: Decodable {
    let entries: [FeedReceipt]
    let nextCursor: FeedCursor?
}

/// Every workflow, one page at a time.
struct FeedRunsRequest: Encodable {
    let cursor: FeedCursor?
    let limit: Int

    private enum WireKey: String, CodingKey {
        case workflowId = "workflow_id"
        case cursor
        case limit
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encodeNil(forKey: .workflowId)
        try container.encode(cursor, forKey: .cursor)
        try container.encode(limit, forKey: .limit)
    }
}

struct FeedRunsParams: Encodable {
    let request: FeedRunsRequest
}

/// A promise somebody made out loud that nothing has answered yet.
struct FeedOpenLoop: Decodable, Identifiable {
    let loopId: String
    let meetingId: String
    let title: String
    let atUtcMs: Int64
    let text: String

    var id: String { loopId }
}

struct FeedOpenLoops: Decodable {
    let entries: [FeedOpenLoop]
}

struct FeedLimitParams: Encodable {
    let limit: Int
}

/// Wall-clock milliseconds, as the meeting store keeps them, and how long ago
/// that was in words.
enum FeedClock {
    private static let relative: RelativeDateTimeFormatter = {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        formatter.dateTimeStyle = .named
        return formatter
    }()

    static func date(_ milliseconds: Int64) -> Date {
        Date(timeIntervalSince1970: TimeInterval(milliseconds) / 1000)
    }

    /// "2 minutes ago", "yesterday", or the day and time once it is older
    /// than a week.
    static func ago(_ milliseconds: Int64) -> String {
        let moment = date(milliseconds)
        let elapsed = Date.now.timeIntervalSince(moment)
        if elapsed >= 7 * 24 * 3600 {
            return "\(moment.short), \(moment.time)"
        }
        return relative.localizedString(for: moment, relativeTo: .now)
    }

    static func isToday(_ milliseconds: Int64) -> Bool {
        Calendar.current.isDateInToday(date(milliseconds))
    }
}

// MARK: - Upcoming

/// `meeting_upcoming_events`: the window that was read, what is in it, and
/// why it might be empty.
struct OverviewUpcoming: Decodable {
    let access: String
    var rows: [OverviewUpcomingRow]
    /// The fence every series write from these rows carries. One number for
    /// the whole pane, because one counter fences all three decisions.
    var seriesRevision: Int

    /// What an empty list means. An empty week under `authorized` is a free
    /// week; anything else is a missing grant.
    var emptyLine: String {
        switch access {
        case "authorized": "Nothing in the next seven days."
        case "not_determined": "Sona has not asked for your calendar yet. Turn meeting detection on in Settings to list what is coming."
        case "denied": "Calendar access is off, so upcoming meetings cannot be listed."
        default: "This Mac has no calendar Sona can read."
        }
    }

    /// Takes the answer to a series write. Two occurrences of one series can
    /// sit in the same week, so every row with that key is replaced rather
    /// than the row that was pressed, and the fence moves to the value the
    /// core just stored — including when the core rejected the write and
    /// answered with what is actually stored.
    mutating func patch(_ write: OverviewSeriesWrite) {
        seriesRevision = write.revision
        for index in rows.indices where rows[index].series?.seriesKey == write.seriesKey {
            rows[index].series = write.series
        }
    }
}

struct OverviewUpcomingRow: Decodable, Identifiable {
    let eventKey: String
    let title: String
    let startUtcMs: Int64
    let endUtcMs: Int64
    let attendeeCount: Int
    let calendarName: String?
    let joinUrl: String?
    /// Present exactly when the event repeats: a one-off has no series to
    /// remember anything, so it offers no standing decisions.
    var series: OverviewSeries?

    var id: String { eventKey }

    /// "Today 14:30–15:00 · Work · 4 people".
    var meta: String {
        let start = FeedClock.date(startUtcMs)
        let end = FeedClock.date(endUtcMs)
        var parts = ["\(start.relativeDay) \(start.time)–\(end.time)"]
        if let calendarName, !calendarName.isEmpty {
            parts.append(calendarName)
        }
        if attendeeCount > 0 {
            parts.append(attendeeCount == 1 ? "1 person" : "\(attendeeCount) people")
        }
        return parts.joined(separator: " · ")
    }
}

/// What one series has decided, joined onto every row that belongs to it.
struct OverviewSeries: Decodable, Equatable {
    let seriesKey: String
    var alwaysRecord: Bool
    var template: OverviewTemplate?
    var digestIncluded: Bool
}

/// The notes template a series is remembered by, `nil` for the app default.
///
/// Decoded strictly. The core's list is closed, so a value this shell does
/// not know means the two halves disagree about the wire; losing the section
/// says that, while quietly drawing "App default" over a template the core is
/// actually using would not.
enum OverviewTemplate: String, CaseIterable, Decodable {
    case general
    case oneOnOne = "one_on_one"
    case interview
    case salesCall = "sales_call"
    case standup

    var label: String {
        switch self {
        case .general: "General meeting"
        case .oneOnOne: "One-to-one"
        case .interview: "Interview"
        case .salesCall: "Sales call"
        case .standup: "Standup"
        }
    }
}

/// What a series write answered: the record the core now holds, and the new
/// value of the pane's fence.
///
/// Meeting settings owns the three commands and the consent receipt the
/// always-record grant writes, and answers with its own mutation type. The
/// integrator maps that answer into this, so the capture page depends on the
/// shape of the answer rather than on another slice's types.
struct OverviewSeriesWrite {
    let seriesKey: String
    let series: OverviewSeries
    let revision: Int
}

/// The three standing decisions an upcoming row offers, supplied by the
/// integrator from the Meeting settings store.
///
/// Optional, and the controls are drawn only when it is supplied: a switch
/// with nothing behind it is worse than no switch. Each call carries the
/// fence the pane is holding and answers with the stored record.
struct OverviewSeriesActions {
    var setAlwaysRecord: @MainActor (String, Bool, Int) async throws -> OverviewSeriesWrite
    var setTemplate: @MainActor (String, OverviewTemplate?, Int) async throws -> OverviewSeriesWrite
    var setDigestIncluded: @MainActor (String, Bool, Int) async throws -> OverviewSeriesWrite

    init(
        setAlwaysRecord: @escaping @MainActor (String, Bool, Int) async throws -> OverviewSeriesWrite,
        setTemplate: @escaping @MainActor (String, OverviewTemplate?, Int) async throws -> OverviewSeriesWrite,
        setDigestIncluded: @escaping @MainActor (String, Bool, Int) async throws -> OverviewSeriesWrite
    ) {
        self.setAlwaysRecord = setAlwaysRecord
        self.setTemplate = setTemplate
        self.setDigestIncluded = setDigestIncluded
    }
}

struct OverviewUpcomingParams: Encodable {
    let days: Int
}

// MARK: - Update

/// `check_for_updates`. A check that was never made and a check that found
/// nothing are different answers, so `status` is kept as the core sent it.
struct OverviewUpdate: Decodable {
    let currentVersion: String
    let latestVersion: String?
    let url: String?
    let status: String

    var waiting: Bool { status == "update_available" }

    var sentence: String {
        "Sona \(latestVersion ?? "") is out. You are on \(currentVersion)."
    }
}

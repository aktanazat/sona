import Foundation

/// The saved prompt library, as `src-tauri/src/meeting/prompt_types.rs` sends
/// it, plus the post-processing prompt library the core keeps in its settings.
/// Field names are the Rust ones with snake_case turned into camelCase by
/// `Core.decoder`; enum values are the wire strings, verbatim.

extension CoreEvent {
    /// The core rewrote its settings file. The dictation prompt library lives
    /// there, so the library re-reads `get_app_settings` when this arrives.
    static let promptSettingsChanged = "settings-changed"
}

/// Which noun a prompt is written about.
enum PromptTarget: String, CaseIterable, Codable, Hashable, Identifiable {
    case meeting
    case person
    case series

    var id: String { rawValue }

    /// "A meeting", the way the editor's About field names it.
    var label: String {
        switch self {
        case .meeting: "A meeting"
        case .person: "A person"
        case .series: "A series"
        }
    }

    /// The plural heading over a list of these to run against.
    var plural: String {
        switch self {
        case .meeting: "Meetings"
        case .person: "People"
        case .series: "Series"
        }
    }
}

/// The noun one run is about: the kind and that kind's own id field.
struct PromptTargetRef: Encodable, Hashable {
    let kind: PromptTarget
    let id: String

    private enum Key: String, CodingKey {
        case kind
        case sessionId = "session_id"
        case personId = "person_id"
        case seriesKey = "series_key"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(kind.rawValue, forKey: .kind)
        switch kind {
        case .meeting: try container.encode(id, forKey: .sessionId)
        case .person: try container.encode(id, forKey: .personId)
        case .series: try container.encode(id, forKey: .seriesKey)
        }
    }
}

/// What shape a prompt's answer takes: prose, or one JSON object checked
/// against a schema before it is stored.
enum PromptOutput: Codable, Hashable {
    case text
    case schema(String)

    private enum Key: String, CodingKey {
        case kind
        /// `json_schema`, as the decoder hands it over.
        case jsonSchema
    }

    private enum EncodeKey: String, CodingKey {
        case kind
        case jsonSchema = "json_schema"
    }

    var schemaText: String? {
        if case let .schema(text) = self { return text }
        return nil
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "text": self = .text
        case "schema": self = .schema(try container.decode(String.self, forKey: .jsonSchema))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown prompt output \(kind)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: EncodeKey.self)
        switch self {
        case .text:
            try container.encode("text", forKey: .kind)
        case let .schema(text):
            try container.encode("schema", forKey: .kind)
            try container.encode(text, forKey: .jsonSchema)
        }
    }
}

/// One question this Mac keeps. The three that shipped are rows in the same
/// table as one typed this morning, and nothing here can tell them apart.
struct SavedPrompt: Decodable, Identifiable, Hashable {
    let promptId: String
    let name: String
    let body: String
    let output: PromptOutput
    let target: PromptTarget
    let createdAtUtcMs: Double
    let updatedAtUtcMs: Double

    var id: String { promptId }
}

/// The library and the revision every write has to carry back.
struct SavedPromptList: Decodable {
    let prompts: [SavedPrompt]
    let revision: UInt64
}

/// The part of the fenced write receipt a screen reads: `committed`,
/// `rejected` or `failed`.
struct PromptOperationReceipt: Decodable {
    let result: String

    var rejected: Bool { result == "rejected" }
}

struct SavedPromptMutationResult: Decodable {
    let receipt: PromptOperationReceipt
    let prompts: SavedPromptList
}

/// Why a run produced no answer.
enum PromptRunFailure: String, Decodable {
    case modelUnavailable = "model_unavailable"
    case modelUnreachable = "model_unreachable"
    case modelFailed = "model_failed"
    case schemaMismatch = "schema_mismatch"
    case noEvidence = "no_evidence"

    var sentence: String {
        switch self {
        case .modelUnavailable: "No model was available to answer this."
        case .modelUnreachable: "Your server could not be reached."
        case .modelFailed: "The model did not return a usable answer."
        case .schemaMismatch: "The answer did not match the schema."
        case .noEvidence: "There was nothing recorded to answer from."
        }
    }
}

/// What one run produced.
enum PromptRunResult: Decodable, Hashable {
    case text(String)
    case json(String)
    case failed(PromptRunFailure)

    private enum Key: String, CodingKey { case kind, text, json, reason }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "text": self = .text(try container.decode(String.self, forKey: .text))
        case "json": self = .json(try container.decode(String.self, forKey: .json))
        case "failed": self = .failed(try container.decode(PromptRunFailure.self, forKey: .reason))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown run result \(kind)")
        }
    }
}

/// One attempt at one prompt, and its receipt. Nothing retries: a failed run
/// is the answer, and it stays visible.
struct PromptRun: Decodable, Identifiable, Hashable {
    let runId: String
    let promptId: String
    let targetKind: PromptTarget
    let targetId: String
    let modelId: String
    let modelVersion: String
    let producedAtUtcMs: Double
    let result: PromptRunResult

    var id: String { runId }
}

/// One post-processing prompt: the rewrite instruction Sona sends to the LLM
/// after a dictation. `LLMPrompt` in the bindings.
struct PromptDictationEntry: Decodable, Identifiable, Hashable {
    let id: String
    let name: String
    let prompt: String
}

/// The two fields of the core's settings this screen reads.
struct PromptAppSettings: Decodable {
    let postProcessPrompts: [PromptDictationEntry]?
    let postProcessSelectedPromptId: String?
}

/// The command errors these screens say something specific about. Anything
/// else falls through to the core's own words.
enum PromptCommandError: String, Decodable {
    case notFound = "not_found"
    case invalidRequest = "invalid_request"
    case staleRevision = "stale_revision"
    case storageUnavailable = "storage_unavailable"
    case recoveryRequired = "recovery_required"
    case deletionInProgress = "deletion_in_progress"
    case localModelUnavailable = "local_model_unavailable"
    case remoteUnavailable = "remote_unavailable"
    case engineFailure = "engine_failure"

    var sentence: String {
        switch self {
        case .notFound: "That prompt is gone, or there is nothing to ask about yet."
        case .invalidRequest: "A prompt needs a name, a prompt, and a schema that is JSON."
        case .staleRevision: "Another window changed these. Read again and retry."
        case .storageUnavailable: "Encrypted meeting storage is unavailable."
        case .recoveryRequired: "The meeting store needs recovery before that."
        case .deletionInProgress: "A deletion is in progress. Try again once it finishes."
        case .localModelUnavailable: "No local model is available for that."
        case .remoteUnavailable: "The selected remote destination is unavailable."
        case .engineFailure: "The engine failed on that request."
        }
    }
}

/// What to show for anything a command refused to do: the core's own error
/// code as a sentence, or its own words when the code is one this screen has
/// nothing better to say about.
func promptErrorSentence(_ error: Error) -> String {
    if let core = error as? CoreError {
        if let known = core.remote(as: PromptCommandError.self) {
            return known.sentence
        }
        return core.localizedDescription
    }
    return error.localizedDescription
}

/// One record a prompt can be run against, as the picker shows it.
struct PromptTargetOption: Identifiable, Hashable {
    let kind: PromptTarget
    let recordId: String
    let title: String
    /// The quiet line beside the title: a date, a meeting count.
    let detail: String

    var id: String { "\(kind.rawValue):\(recordId)" }
    var ref: PromptTargetRef { PromptTargetRef(kind: kind, id: recordId) }
}

/// One page of meetings, as `meeting_list` sends it.
struct PromptMeetingPage: Decodable {
    struct Row: Decodable {
        let sessionId: String
        let title: String
        let createdAtUtcMs: Double
    }

    let entries: [Row]
}

/// The people this Mac knows, as `people_list` sends it.
struct PromptPeopleList: Decodable {
    struct Person: Decodable {
        let id: String
        let displayName: String
    }

    struct Row: Decodable {
        let person: Person
        let meetingsCount: Int
    }

    let entries: [Row]
}

/// The recurring meetings, as `meeting_automation_roster` sends them: the one
/// command that hands back series keys with names on them.
struct PromptSeriesRoster: Decodable {
    struct Row: Decodable {
        let seriesKey: String
        let title: String
        let meetingCount: Int
        let lastMetAtUtcMs: Double
    }

    let series: [Row]
}

/// One key and its value out of a schema answer.
struct PromptAnswerRow: Identifiable, Hashable {
    let key: String
    let value: String

    var id: String { key }
}

/// A stored schema answer, as rows.
///
/// The store only keeps JSON that already checked against the prompt's schema,
/// so this reads what is there rather than checking again — and it reads
/// defensively, because this is text a model wrote and a database kept. Keys
/// come back sorted: a JSON object has no order once it is parsed.
func promptAnswerRows(_ json: String) -> [PromptAnswerRow] {
    guard let data = json.data(using: .utf8),
        let value = try? JSONDecoder().decode(JSONValue.self, from: data),
        case let .object(fields) = value
    else {
        return []
    }
    return fields.keys.sorted().map { key in
        PromptAnswerRow(key: key, value: promptCellText(fields[key] ?? .null))
    }
}

/// One cell, read as the text it shows. The schema subset this app enforces
/// describes strings, numbers, booleans and lists of those; anything deeper is
/// printed as JSON rather than flattened into something it is not.
func promptCellText(_ value: JSONValue) -> String {
    switch value {
    case let .string(text): text
    case let .bool(flag): flag ? "true" : "false"
    case let .number(number):
        number == number.rounded() && abs(number) < 1e15
            ? String(Int64(number))
            : String(number)
    case let .array(items): items.map(promptCellText).joined(separator: ", ")
    case .null: ""
    case .object: value.message
    }
}

/// "3 minutes ago", "yesterday", and an absolute date past a fortnight — the
/// steps `src/lib/utils/format.ts` uses, in the same order.
func promptRelativeTime(_ utcMs: Double, now: Date = .now) -> String {
    let elapsed = max(0, now.timeIntervalSince1970 - utcMs / 1000)
    let moment = Date(timeIntervalSince1970: utcMs / 1000)
    if elapsed < 3600 {
        return promptRelativeFormatter.localizedString(from: DateComponents(minute: -Int(elapsed / 60)))
    }
    if elapsed < 86_400 {
        return promptRelativeFormatter.localizedString(from: DateComponents(hour: -Int(elapsed / 3600)))
    }
    if elapsed < 86_400 * 14 {
        return promptRelativeFormatter.localizedString(from: DateComponents(day: -Int(elapsed / 86_400)))
    }
    let sameYear = Calendar.current.component(.year, from: moment) == Calendar.current.component(.year, from: now)
    return sameYear ? "\(moment.short), \(moment.time)" : "\(moment.short) \(Calendar.current.component(.year, from: moment)), \(moment.time)"
}

/// "yesterday" rather than "1 day ago", the way `numeric: "auto"` reads.
let promptRelativeFormatter: RelativeDateTimeFormatter = {
    let formatter = RelativeDateTimeFormatter()
    formatter.dateTimeStyle = .named
    formatter.unitsStyle = .full
    return formatter
}()

/// A count and its noun, the way the run copy plurals: "1 person", "2 people".
func promptCounted(_ count: Int, _ singular: String, _ plural: String) -> String {
    count == 1 ? "1 \(singular)" : "\(count) \(plural)"
}

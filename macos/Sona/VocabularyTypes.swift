import Foundation

/// The shapes behind the text rules Sona applies after a transcript: spellings
/// (`custom_words`), shortcuts (`snippets`), rewrites (`replacements_rules`),
/// emoji pairs (`emoji_replacements`), and the writing samples a rewrite is
/// shown as voice-matching examples. Field names mirror the Rust structs in
/// `src-tauri/src/settings.rs` and `src-tauri/src/snippets.rs`, with
/// snake_case turned into camelCase by `Core.decoder`.

/// `settings-changed` carries no useful payload, so the store answers it by
/// re-reading `get_app_settings`.
extension CoreEvent {
    static let vocabularySettingsChanged = "settings-changed"
}

/// Which vocabulary list a command addresses. Every command on this surface
/// addresses the global list; a mode's own list belongs to the mode editor.
struct VocabularyScope: Encodable {
    let kind: String

    static let global = VocabularyScope(kind: "global")
}

/// The spoken/written shape three of the four stores share, so one draft-merge
/// rule can serve all of them.
protocol VocabularyPair {
    var spoken: String { get }
    var written: String { get }
}

/// A deterministic correction from what the recognizer heard to what should be
/// written.
struct VocabularyEntry: Codable, Equatable, VocabularyPair {
    var spoken: String
    var written: String
}

/// An opt-in exact-token replacement applied after vocabulary correction.
struct EmojiReplacement: Codable, Equatable, VocabularyPair {
    var spoken: String
    var written: String
}

/// A spoken-phrase rewrite applied before vocabulary correction. Unlike a
/// spelling it does not bias the recognizer: it rewrites what was heard.
struct TextReplacementRule: Codable, Equatable, VocabularyPair {
    var spoken: String
    var written: String
    var enabled: Bool
}

/// A user-authored text expansion, matched on whole words after vocabulary
/// correction.
struct SnippetRecord: Codable, Equatable, Identifiable {
    let id: String
    var trigger: String
    var expansion: String
    var enabled: Bool
    let createdAt: Int64
    let updatedAt: Int64

    /// A snippet the core has not seen. An empty id asks for a new record; the
    /// clocks travel because the wire type requires them and the core
    /// overwrites both with its own.
    static func draft(trigger: String, expansion: String) -> SnippetRecord {
        SnippetRecord(
            id: "",
            trigger: trigger,
            expansion: expansion,
            enabled: true,
            createdAt: 0,
            updatedAt: 0
        )
    }

    /// Requests are encoded with a plain encoder, so the two clock fields have
    /// to carry the Rust names themselves; only replies get case conversion.
    private enum WireKey: String, CodingKey {
        case id, trigger, expansion, enabled
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: WireKey.self)
        try container.encode(id, forKey: .id)
        try container.encode(trigger, forKey: .trigger)
        try container.encode(expansion, forKey: .expansion)
        try container.encode(enabled, forKey: .enabled)
        try container.encode(createdAt, forKey: .createdAt)
        try container.encode(updatedAt, forKey: .updatedAt)
    }
}

/// One snippet row mid-edit. Snippets are the one store written per record, so
/// an untouched row must have nothing to send.
struct SnippetDraft: Equatable {
    var trigger: String
    var expansion: String
}

/// One paragraph of the user's own writing, shown to a rewrite as a
/// voice-matching example.
struct PersonaSample: Codable, Equatable, Identifiable {
    let id: String
    var text: String

    /// What the core counts when it truncates: whitespace-separated words.
    var wordCount: Int {
        text.split(whereSeparator: \.isWhitespace).count
    }
}

/// Few-shot prompting degrades once the examples crowd out the transcript, so
/// the core bounds the set on both axes and the editor names both bounds.
enum PersonaSampleLimit {
    static let count = 5
    static let words = 500
}

/// What a CSV would do to the saved list, before anything is written.
struct VocabularyCsvPreview: Decodable {
    let totalRows: Int
    let validRows: Int
    let invalidRows: Int
    let duplicateRows: Int
    let conflictRows: Int
    let canApply: Bool
    let entries: [VocabularyEntry]
}

/// An import is read first and confirmed second, because applying replaces the
/// saved list.
enum VocabularyImportStep {
    case review
    case confirm
}

struct VocabularyImportReview {
    let csv: String
    let preview: VocabularyCsvPreview
    var step: VocabularyImportStep
}

/// The four stores, as the word that names a row's kind.
enum VocabularyRuleKind: String, CaseIterable, Hashable {
    case vocabulary
    case snippet
    case replacement
    case emoji

    /// The kind as a word on the row, never a coloured pill: this list is long
    /// enough that four saturated chips a screen would read as decoration.
    var title: String {
        switch self {
        case .vocabulary: "Spelling"
        case .snippet: "Shortcut"
        case .replacement: "Rewrite"
        case .emoji: "Emoji"
        }
    }

    var hint: String {
        switch self {
        case .vocabulary:
            "Teaches Sona how a name is spelled, so it writes the exact form every time it hears the phrase."
        case .snippet:
            "Expands a short trigger into longer text, such as omw into on my way."
        case .replacement:
            "Rewrites a phrase Sona already heard, such as at sign into @."
        case .emoji:
            "Maps an exact spoken token, such as smiley face, to the emoji you want written."
        }
    }

    var spokenExample: String {
        switch self {
        case .vocabulary: "open ai"
        case .snippet: "omw"
        case .replacement: "at sign"
        case .emoji: "smiley face"
        }
    }

    var writtenExample: String {
        switch self {
        case .vocabulary: "OpenAI"
        case .snippet: "on my way"
        case .replacement: "@"
        case .emoji: "🙂"
        }
    }
}

/// Which store owns a row, and where in it. Three stores address a rule by its
/// position in the list they take whole; snippets have their own record ids.
/// Every mutation switches on this, so a rewrite can never write into the
/// spelling list.
enum VocabularyRuleAddress: Equatable {
    case vocabulary(Int)
    case emoji(Int)
    case replacement(Int)
    case snippet(String)
}

enum VocabularyRuleSide {
    case left
    case right
}

/// One row of the merged list: the kind is on the row rather than in a section
/// heading, so a reader asking "why did it write that" reads one list.
struct VocabularyRule: Identifiable, Equatable {
    /// `kind:address` — the row's identity, and nothing more.
    let id: String
    let kind: VocabularyRuleKind
    let address: VocabularyRuleAddress
    /// What the person says.
    var left: String
    /// What Sona writes.
    var right: String
    /// `nil` for the stores with no per-rule switch.
    var enabled: Bool?
}

/// The part of `get_app_settings` this surface reads. The four switches and
/// the two lists the settings file owns.
struct VocabularySettingsSnapshot: Decodable {
    let customWords: [VocabularyEntry]?
    let emojiReplacements: [EmojiReplacement]?
    let emojiReplacementsEnabled: Bool?
    let snippetsEnabled: Bool?
    let replacementsEnabled: Bool?
    let spokenEditsEnabled: Bool?
}

/// The keys the core matches by, so the editor can refuse exactly what the
/// core would refuse instead of forwarding a doomed write.
enum VocabularyKey {
    /// Mirrors `vocabulary_spoken_key`: letters and numbers only, lowercased,
    /// which is what makes "Open AI" and "open-ai" the same rule.
    static func spoken(_ text: String) -> String {
        var key = String.UnicodeScalarView()
        for scalar in text.unicodeScalars
        where scalar.properties.isAlphabetic || scalar.properties.numericType != nil {
            key.append(scalar)
        }
        return String(key).lowercased()
    }

    /// Mirrors `trigger_key`: snippets are unique after case folding.
    static func trigger(_ text: String) -> String {
        trimmed(text).lowercased()
    }

    static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// When a list the core owns may replace the list under the cursor, and what a
/// CSV apply must not discard.
enum VocabularyDraft {
    private static func pairKey(_ pair: some VocabularyPair) -> String {
        "\(pair.spoken)\u{0}\(pair.written)"
    }

    static func samePairs(_ left: [some VocabularyPair], _ right: [some VocabularyPair]) -> Bool {
        guard left.count == right.count else {
            return false
        }
        for (index, pair) in left.enumerated() where pairKey(pair) != pairKey(right[index]) {
            return false
        }
        return true
    }

    /// A settings refresh may replace the local list outright only while
    /// nothing local has diverged since the last synced snapshot. Mid-edit,
    /// the list stays as it is, because every edit, focus and save addresses a
    /// row by its position and an outside removal would move the row under
    /// the cursor. The newest saved list waits in `pending` for `resolveSave`.
    static func resolveRefresh<T: VocabularyPair>(
        current: [T],
        synced: [T],
        incoming: [T]
    ) -> (rows: [T], synced: [T], pending: [T]?) {
        samePairs(current, synced)
            ? (incoming, incoming, nil)
            : (current, synced, incoming)
    }

    /// The whole-list save that ends an edit keeps the rows changed here and
    /// lets every other row follow what was saved from outside meanwhile:
    /// outside changes and removals land, outside additions are appended.
    static func resolveSave<T: VocabularyPair>(_ next: [T], synced: [T], pending: [T]?) -> [T] {
        guard let pending else {
            return next
        }
        return merge(
            current: next,
            previousSaved: synced,
            incomingSaved: pending,
            key: { VocabularyKey.spoken($0.spoken) },
            same: { pairKey($0) == pairKey($1) }
        )
    }

    static func merge<Row, Key: Hashable>(
        current: [Row],
        previousSaved: [Row],
        incomingSaved: [Row],
        key: (Row) -> Key,
        same: (Row, Row) -> Bool
    ) -> [Row] {
        let before = Dictionary(previousSaved.map { (key($0), $0) }, uniquingKeysWith: { first, _ in first })
        let after = Dictionary(incomingSaved.map { (key($0), $0) }, uniquingKeysWith: { first, _ in first })
        var merged: [Row] = current.compactMap { row in
            guard let saved = before[key(row)], same(saved, row) else {
                return row
            }
            return after[key(row)]
        }
        let present = Set(merged.map(key))
        merged += incomingSaved.filter { before[key($0)] == nil && !present.contains(key($0)) }
        return merged
    }

    /// The core adds the CSV rows to the saved list and answers with that
    /// list, so rows typed locally and never saved are absent from the
    /// answer. Keep every local row the CSV does not also define.
    static func mergeAppliedCsv(
        _ localDrafts: [VocabularyEntry],
        _ applied: [VocabularyEntry]
    ) -> [VocabularyEntry] {
        let appliedPairs = Set(applied.map(pairKey))
        return applied + localDrafts.filter { !appliedPairs.contains(pairKey($0)) }
    }
}

/// A name Sona keeps hearing in meetings that no spelling covers yet: a word
/// capitalized where grammar does not ask for a capital.
struct HeardWord: Decodable, Identifiable {
    let text: String
    let occurrences: UInt64
    let meetingsCount: UInt64

    var id: String { text }
}

struct HeardWords: Decodable {
    let schemaVersion: UInt32
    let revision: UInt64
    let entries: [HeardWord]
}

/// An answer to one heard word. The core keeps it, so a dismissed word stays
/// quiet on every surface that mines it.
///
/// The key is normalized where it lands and cannot be shown back to anybody,
/// so the display form travels beside it.
struct HeardWordDecision: Encodable {
    let candidateKey: String
    let status: String

    private enum CodingKeys: String, CodingKey {
        case loopKind = "loop_kind"
        case candidateKey = "candidate_key"
        case status
        case displayText = "display_text"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode("vocabulary_term", forKey: .loopKind)
        try container.encode(candidateKey, forKey: .candidateKey)
        try container.encode(status, forKey: .status)
        try container.encode(candidateKey, forKey: .displayText)
    }
}

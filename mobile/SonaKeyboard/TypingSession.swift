import UIKit

/// The keyboard's memory between keys: what the last key did, so backspace can undo a
/// correction, a second space can end a sentence and a second shift can lock caps, and
/// which words the user kept and learned.
@MainActor
struct TypingSession {
    /// A change to the text before the cursor: delete this many characters, then insert.
    struct Edit: Equatable {
        var deleteCount = 0
        var insert = ""
    }

    private enum Last: Equatable {
        case other
        case space
        case chosen
        case corrected(Correction, separator: String)
        case shift(Date)
        case swiped(String, spaced: Bool, others: [String])

        var endsWithSpace: Bool {
            switch self {
            case .space, .chosen: true
            case let .corrected(_, separator): separator == " "
            case .other, .shift, .swiped: false
            }
        }
    }

    /// A correction the strip can take back, kept for the one place it was made: its field,
    /// and the text before the cursor right after the corrected word.
    private struct Corrected: Equatable {
        let correction: Correction
        let field: UUID?
        let place: String
    }

    /// Corrections kept for the cursor coming back to them; older ones are forgotten.
    private static let correctionMemory = 30

    var lexicon: TypingLexicon
    private(set) var learning: KeyboardLearning
    var predictor: NextWords?
    private let corrector: TypingCorrector?
    private let emoji: (String) -> String?
    private var last = Last.other
    /// The text before the cursor as the last key left it. When the cursor moves, the field
    /// changes or dictated text lands, the text no longer matches and the last key is forgotten.
    private var textAfterLast = ""
    private var corrections: [Corrected] = []
    /// The field being typed in, and its text around the cursor as the keyboard last saw it.
    private var field: UUID?
    private var fieldText: String?

    init(
        corrector: TypingCorrector?, lexicon: TypingLexicon = TypingLexicon(),
        learning: KeyboardLearning = KeyboardLearning(), predictor: NextWords? = nil,
        emoji: @escaping (String) -> String? = { _ in nil }
    ) {
        self.corrector = corrector
        self.lexicon = lexicon
        self.learning = learning
        self.predictor = predictor
        self.emoji = emoji
        self.lexicon.setLearned(learning.known)
    }

    /// What the strip offers at the cursor: right after a swipe, the other words the path
    /// could have meant; at the end of a word corrected earlier, the word as it was typed;
    /// while a word is typed, its corrections, completions and emoji; after a space, the
    /// words likely to come next. Without `guessing`, a word being typed gets completions and
    /// emoji but no spelling guesses.
    func suggestions(before text: String, guessing: Bool = true) -> Suggestions? {
        if case let .swiped(word, _, others) = lastKey(before: text), !others.isEmpty {
            return Suggestions(typed: word, correction: nil, others: others)
        }
        guard let word = TypingText.word(before: text) else { return predictions(before: text) }
        if let corrected = corrections.last(where: { $0.field == field && $0.place == text }) {
            return Suggestions(typed: String(word), correction: nil, others: [], restore: corrected.correction.typed)
        }
        let found = emoji(String(word))
        guard var offer = corrector?.suggestions(before: text, lexicon: lexicon, guessing: guessing) else {
            return found.map { Suggestions(typed: String(word), correction: nil, others: [], emoji: $0) }
        }
        offer.emoji = found
        return offer
    }

    private func predictions(before text: String) -> Suggestions? {
        guard text.isEmpty || text.last?.isWhitespace == true else { return nil }
        let context = TypingText.sentenceWords(before: text[...], limit: 2)
        let own = context.last.map { learning.next(after: $0, limit: 3) } ?? []
        var seen: Set<String> = []
        let words = (own + (predictor?.next(after: context, limit: 3) ?? []))
            .filter { seen.insert($0.lowercased()).inserted }
        guard !words.isEmpty else { return nil }
        return Suggestions(typed: "", correction: nil, others: Array(words.prefix(3)))
    }

    /// The space a word dropped in whole, swiped or dictated, needs in front of it: none at
    /// the start of the text, after a space or after an opening bracket or quote.
    static func joiningSpace(before text: String) -> String {
        guard let last = text.last, !last.isWhitespace, !"([{\"“‘/-".contains(last) else { return "" }
        return " "
    }

    /// The best word a swipe spelled, spaced from the word before it as the system keyboard
    /// does; `words` are already cased for where they land.
    mutating func swipe(_ words: [String], before text: String) -> Edit {
        guard let word = words.first else { return Edit() }
        let space = Self.joiningSpace(before: text)
        return remember(
            .swiped(word, spaced: !space.isEmpty, others: Array(words.dropFirst())),
            Edit(insert: space + word), on: text
        )
    }

    /// A letter or symbol that keeps the current word going.
    mutating func type(_ text: String) -> Edit {
        last = .other
        return Edit(insert: text)
    }

    /// Space, return or punctuation, which ends the word before the cursor and corrects it
    /// when the field allows. A second space after a word ends the sentence. Where the field
    /// allows correction, the word is also learned. A host still behind the keyboard's last
    /// edit gets only plain edits: nothing is corrected, replaced or deleted beyond one
    /// character on text it may not hold.
    mutating func end(with separator: String, before text: String, corrects: Bool, hostBehind: Bool = false) -> Edit {
        if hostBehind { return remember(.other, Edit(insert: separator), on: text) }
        let previous = lastKey(before: text)
        if separator == " ", previous.endsWithSpace, Self.endsWord(text) {
            return remember(.other, Edit(deleteCount: 1, insert: ". "), on: text)
        }
        if previous == .chosen, ".,!?;:".contains(separator), text.hasSuffix(" ") {
            return remember(.other, Edit(deleteCount: 1, insert: separator + " "), on: text)
        }
        if case .swiped = previous {
            if corrects { learn(before: text) }
            return remember(separator == " " ? .space : .other, Edit(insert: separator), on: text)
        }
        if corrects, let correction = corrector?.correction(before: text, lexicon: lexicon) {
            learn(before: text, as: correction.replacement)
            let place = String(text.dropLast(correction.typed.count)) + correction.replacement
            corrections = Array((corrections + [Corrected(correction: correction, field: field, place: place)])
                .suffix(Self.correctionMemory))
            let edit = Edit(deleteCount: correction.typed.count, insert: correction.replacement + separator)
            return remember(.corrected(correction, separator: separator), edit, on: text)
        }
        if corrects { learn(before: text) }
        return remember(separator == " " ? .space : .other, Edit(insert: separator), on: text)
    }

    /// Right after a correction, backspace puts the typed word back and keeps it from then
    /// on; right after a swipe it deletes the whole word and its space; otherwise it deletes
    /// one character.
    mutating func backspace(before text: String, hostBehind: Bool = false) -> Edit {
        if hostBehind { return remember(.other, Edit(deleteCount: 1), on: text) }
        switch lastKey(before: text) {
        case let .corrected(correction, separator):
            keep(correction.typed)
            corrections.removeAll { $0.correction == correction }
            let edit = Edit(deleteCount: (correction.replacement + separator).count, insert: correction.typed)
            return remember(.other, edit, on: text)
        case let .swiped(word, spaced, _):
            return remember(.other, Edit(deleteCount: word.count + (spaced ? 1 : 0)), on: text)
        case .other, .space, .chosen, .shift:
            return remember(.other, Edit(deleteCount: 1), on: text)
        }
    }

    /// The whole word before the cursor and the spaces after it, as a slide left on delete
    /// takes them; one character when no word is there.
    static func wordDeletion(before text: String) -> Int {
        let trailing = text.reversed().prefix { $0.isWhitespace }.count
        let rest = text.dropLast(trailing)
        guard let last = rest.last else { return trailing }
        let word = TypingText.isWordCharacter(last) || last.isNumber
            ? rest.reversed().prefix { TypingText.isWordCharacter($0) || $0.isNumber }.count
            : 1
        return trailing + word
    }

    /// A strip candidate replaces the word before the cursor and adds a space. Choosing the
    /// word as typed, or the typed form of a word corrected earlier, keeps it from then on.
    /// After a swipe, the candidate swaps in for the swiped word and still goes in one
    /// backspace. With no word at the cursor the candidate is the next word.
    mutating func choose(_ candidate: String, before text: String, hostBehind: Bool = false) -> Edit {
        if hostBehind { return Edit() }
        if case let .swiped(word, spaced, others) = lastKey(before: text) {
            let swapped = [word] + others.filter { $0 != candidate }
            return remember(
                .swiped(candidate, spaced: spaced, others: swapped),
                Edit(deleteCount: word.count, insert: candidate), on: text
            )
        }
        let word = TypingText.word(before: text) ?? ""
        let here = corrections.filter { $0.field == field && $0.place == text }
        if candidate == String(word) || here.contains(where: { $0.correction.typed == candidate }) {
            keep(candidate)
            corrections.removeAll { here.contains($0) }
        }
        return remember(.chosen, Edit(deleteCount: word.count, insert: candidate + " "), on: text)
    }

    /// A shift tap; true when the key before it was shift, tapped less than 0.35 s earlier,
    /// which locks caps.
    mutating func tapShift(at time: Date) -> Bool {
        if case let .shift(previous) = last, time.timeIntervalSince(previous) < 0.35 {
            last = .other
            return true
        }
        last = .shift(time)
        return false
    }

    /// Takes the learning as the shared container has it, emptied when Reset in the app ran.
    mutating func replaceLearning(_ stored: KeyboardLearning) {
        learning = stored
        lexicon.setLearned(stored.known)
    }

    /// The keyboard's own edit landed: `field` now reads `text` around the cursor.
    mutating func edited(field: UUID?, text: String) {
        self.field = field
        fieldText = text
    }

    /// The host reports `field` reading `text` around the cursor. Another field, or text the
    /// keyboard did not write, such as a sent message clearing the field, forgets every
    /// correction the strip could take back; a cursor move alone keeps them.
    mutating func hostChanged(field: UUID?, text: String) {
        if field != self.field || text != fieldText { corrections = [] }
        self.field = field
        fieldText = text
    }

    private mutating func keep(_ word: String) {
        learning.keep(word)
        lexicon.setLearned(learning.known)
    }

    /// Counts the word the cursor ends, or what it is being corrected to, as one more use,
    /// and the word before it as its context. A word that opens a sentence is counted with
    /// its first letter lowered.
    private mutating func learn(before text: String, as replacement: String? = nil) {
        guard let word = TypingText.word(before: text) else { return }
        let ended = replacement.map { Substring($0) } ?? word
        let opening = TypingText.startsSentence(before: text[..<word.startIndex])
        let spelled = opening && ended.dropFirst().allSatisfy(\.isLowercase) ? ended.lowercased() : String(ended)
        let previous = TypingText.sentenceWords(before: text[..<word.startIndex], limit: 1).last
        learning.typed(spelled, after: previous)
        if learning.knows(spelled), !lexicon.contains(spelled) { lexicon.setLearned(learning.known) }
    }

    private func lastKey(before text: String) -> Last {
        text == textAfterLast ? last : .other
    }

    private mutating func remember(_ key: Last, _ edit: Edit, on text: String) -> Edit {
        last = key
        textAfterLast = String(text.dropLast(edit.deleteCount)) + edit.insert
        return edit
    }

    private static func endsWord(_ text: String) -> Bool {
        guard text.last == " ", let previous = text.dropLast().last else { return false }
        return previous.isLetter || previous.isNumber
    }
}

/// The text a word dropped in whole is joined to, for the space in front of it only. Some
/// hosts keep reporting the text from before the keyboard's last edit until the next one
/// comes in, and a word joined to that stale text either glues on ("écoleMeet") or
/// doubles its space ("Dauren  école").
///
/// It never decides what to delete. The keyboard keeps each version of the field from the
/// one the host last showed to its own latest. The host is behind while it shows one of
/// those versions other than the latest; showing any remembered version lets the older ones
/// go, and showing anything else, a change the keyboard did not make, or a new field ends
/// the memory.
///
/// One case is left. An app that both lags and clears the field after Send without a word
/// can show, after the clear, text the memory holds as an older version. The keyboard then
/// takes the app for behind and loses track until the app catches up: dictation gets a
/// stray space, a space can double, and a correction can rewrite stale letters
/// (" Hello I said bbecause  Bye").
struct FieldEcho {
    private struct Report: Equatable {
        let before: String
        let after: String
    }

    /// The versions the host may still show, oldest first; the last is the keyboard's latest.
    /// A host that never updates keeps showing the oldest, so a long burst drops the second.
    private var versions: [Report] = []
    private static let versionLimit = 64

    /// The keyboard changed the field, which the host showed as `before` and `after` the
    /// cursor: it deleted `deleting` characters before the cursor and inserted `inserting`.
    mutating func edited(before: String, after: String, deleting: Int, inserting: String) {
        let shown = Report(before: before, after: after)
        if versions.contains(shown) { advance(to: shown) } else { versions = [shown] }
        let known = versions[versions.count - 1].before
        versions.append(Report(before: String(known.dropLast(deleting)) + inserting, after: after))
        if versions.count > Self.versionLimit { versions.remove(at: 1) }
    }

    /// The host reported the field. A report while the keyboard's own edit is going through
    /// keeps the memory if it shows a remembered version; any other report is a change the
    /// keyboard did not make.
    mutating func hostReported(before: String, after: String, ownEdit: Bool) {
        let shown = Report(before: before, after: after)
        if ownEdit, versions.contains(shown) {
            advance(to: shown)
        } else {
            versions = []
        }
    }

    /// The field the keyboard types into is a different one.
    mutating func forget() {
        versions = []
    }

    /// Whether the host shows a version of the field older than the keyboard's latest, and
    /// no older than the last one it showed.
    func behind(before: String, after: String) -> Bool {
        let shown = Report(before: before, after: after)
        return shown != versions.last && versions.contains(shown)
    }

    /// The text before the cursor to join a word to: the keyboard's latest version while the
    /// host is behind it; otherwise the host's.
    func joining(before: String, after: String) -> String {
        behind(before: before, after: after) ? versions[versions.count - 1].before : before
    }

    private mutating func advance(to shown: Report) {
        if let index = versions.firstIndex(of: shown) { versions.removeFirst(index) }
    }
}

/// The host's field as the keyboard edits it, with `FieldEcho` kept in step.
@MainActor
extension UITextDocumentProxy {
    func apply(_ edit: TypingSession.Edit, echo: inout FieldEcho) {
        echo.edited(
            before: documentContextBeforeInput ?? "", after: documentContextAfterInput ?? "",
            deleting: edit.deleteCount, inserting: edit.insert
        )
        for _ in 0..<edit.deleteCount { deleteBackward() }
        if !edit.insert.isEmpty { insertText(edit.insert) }
    }

    /// What one stretch of a slide on delete takes, returned so sliding back can put it back;
    /// "" when it took something it cannot put back. Selected text goes first and by itself.
    /// A selection longer than 40 characters, or holding a picture, goes in one delete with
    /// nothing to put back: typing it back over itself would be thousands of edits after
    /// Select All, and a picture would come back as a placeholder character.
    /// A short one is typed over itself, which leaves the cursor after it, then deleted a
    /// character at a time: delete on a selection lets a host such as Messages take the space
    /// beside it too, which sliding back could not restore. After that, the word before the
    /// cursor, counted from the text the host shows now; while the host is still behind the
    /// keyboard's last edit it takes nothing.
    func takeWord(echo: inout FieldEcho) -> String? {
        if let selected = selectedText, !selected.isEmpty {
            guard selected.count <= 40, !selected.contains("\u{FFFC}") else {
                deleteBackward()
                return ""
            }
            insertText(selected)
            apply(TypingSession.Edit(deleteCount: selected.count), echo: &echo)
            return selected
        }
        let before = documentContextBeforeInput ?? ""
        guard !echo.behind(before: before, after: documentContextAfterInput ?? "") else { return nil }
        let count = TypingSession.wordDeletion(before: before)
        guard count > 0 else { return nil }
        apply(TypingSession.Edit(deleteCount: count), echo: &echo)
        return String(before.suffix(count))
    }
}

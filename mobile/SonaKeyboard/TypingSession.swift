import Foundation

/// The keyboard's memory between keys: what the last key did, so backspace can undo a
/// correction, a second space can end a sentence and a second shift can lock caps, and
/// which words the user kept.
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

        var endsWithSpace: Bool {
            switch self {
            case .space, .chosen: true
            case let .corrected(_, separator): separator == " "
            case .other, .shift: false
            }
        }
    }

    var lexicon: TypingLexicon
    private let corrector: TypingCorrector?
    private var last = Last.other
    /// The text before the cursor as the last key left it. When the cursor moves, the field
    /// changes or dictated text lands, the text no longer matches and the last key is forgotten.
    private var textAfterLast = ""

    init(corrector: TypingCorrector?, lexicon: TypingLexicon = TypingLexicon()) {
        self.corrector = corrector
        self.lexicon = lexicon
    }

    /// What the strip offers for the word before the cursor.
    func suggestions(before text: String) -> Suggestions? {
        corrector?.suggestions(before: text, lexicon: lexicon)
    }

    /// A letter or symbol that keeps the current word going.
    mutating func type(_ text: String) -> Edit {
        last = .other
        return Edit(insert: text)
    }

    /// Space, return or punctuation, which ends the word before the cursor and corrects it
    /// when the field allows. A second space after a word ends the sentence.
    mutating func end(with separator: String, before text: String, corrects: Bool) -> Edit {
        let previous = lastKey(before: text)
        if separator == " ", previous.endsWithSpace, Self.endsWord(text) {
            return remember(.other, Edit(deleteCount: 1, insert: ". "), on: text)
        }
        if previous == .chosen, ".,!?;:".contains(separator), text.hasSuffix(" ") {
            return remember(.other, Edit(deleteCount: 1, insert: separator + " "), on: text)
        }
        if corrects, let correction = corrector?.correction(before: text, lexicon: lexicon) {
            let edit = Edit(deleteCount: correction.typed.count, insert: correction.replacement + separator)
            return remember(.corrected(correction, separator: separator), edit, on: text)
        }
        return remember(separator == " " ? .space : .other, Edit(insert: separator), on: text)
    }

    /// Right after a correction, backspace puts the typed word back and keeps it for the
    /// rest of the session; otherwise it deletes one character.
    mutating func backspace(before text: String) -> Edit {
        guard case let .corrected(correction, separator) = lastKey(before: text) else {
            return remember(.other, Edit(deleteCount: 1), on: text)
        }
        lexicon.insert(correction.typed)
        let edit = Edit(deleteCount: (correction.replacement + separator).count, insert: correction.typed)
        return remember(.other, edit, on: text)
    }

    /// A strip candidate replaces the word before the cursor and adds a space. Choosing the
    /// word as typed keeps it uncorrected for the rest of the session.
    mutating func choose(_ candidate: String, before text: String) -> Edit {
        let word = TypingText.word(before: text) ?? ""
        if candidate == String(word) { lexicon.insert(candidate) }
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

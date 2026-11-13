import Foundation

/// The keyboard's memory between keys: what the last key did, so backspace can undo a
/// correction and a second space can end a sentence, and which words the user kept.
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
    }

    var lexicon: TypingLexicon
    private let corrector: TypingCorrector?
    private var last = Last.other

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
        if separator == " ", followsSpace, Self.endsWord(text) {
            last = .other
            return Edit(deleteCount: 1, insert: ". ")
        }
        if last == .chosen, ".,!?;:".contains(separator), text.hasSuffix(" ") {
            last = .other
            return Edit(deleteCount: 1, insert: separator + " ")
        }
        if corrects, let correction = corrector?.correction(before: text, lexicon: lexicon) {
            last = .corrected(correction, separator: separator)
            return Edit(deleteCount: correction.typed.count, insert: correction.replacement + separator)
        }
        last = separator == " " ? .space : .other
        return Edit(insert: separator)
    }

    /// Right after a correction, backspace puts the typed word back and keeps it for the
    /// rest of the session; otherwise it deletes one character.
    mutating func backspace(before text: String) -> Edit {
        defer { last = .other }
        guard case let .corrected(correction, separator) = last,
              text.hasSuffix(correction.replacement + separator)
        else { return Edit(deleteCount: 1) }
        lexicon.insert(correction.typed)
        return Edit(deleteCount: (correction.replacement + separator).count, insert: correction.typed)
    }

    /// A strip candidate replaces the word before the cursor and adds a space. Choosing the
    /// word as typed keeps it uncorrected for the rest of the session.
    mutating func choose(_ candidate: String, before text: String) -> Edit {
        let word = TypingText.word(before: text) ?? ""
        if candidate == String(word) { lexicon.insert(candidate) }
        last = .chosen
        return Edit(deleteCount: word.count, insert: candidate + " ")
    }

    private var followsSpace: Bool {
        switch last {
        case .space, .chosen: true
        case let .corrected(_, separator): separator == " "
        case .other: false
        }
    }

    private static func endsWord(_ text: String) -> Bool {
        guard text.last == " ", let previous = text.dropLast().last else { return false }
        return previous.isLetter || previous.isNumber
    }
}

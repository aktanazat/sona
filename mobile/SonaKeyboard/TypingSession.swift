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
        case swiped(String, spaced: Bool, others: [String])

        var endsWithSpace: Bool {
            switch self {
            case .space, .chosen: true
            case let .corrected(_, separator): separator == " "
            case .other, .shift, .swiped: false
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

    /// What the strip offers for the word before the cursor: right after a swipe, the other
    /// words the path could have meant.
    func suggestions(before text: String) -> Suggestions? {
        if case let .swiped(word, _, others) = lastKey(before: text), !others.isEmpty {
            return Suggestions(typed: word, correction: nil, others: others)
        }
        return corrector?.suggestions(before: text, lexicon: lexicon)
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
    /// when the field allows. A second space after a word ends the sentence.
    /// A host still behind the keyboard's last edit gets only plain edits: nothing is
    /// corrected, replaced or deleted beyond one character on text it may not hold.
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
            return remember(separator == " " ? .space : .other, Edit(insert: separator), on: text)
        }
        if corrects, let correction = corrector?.correction(before: text, lexicon: lexicon) {
            let edit = Edit(deleteCount: correction.typed.count, insert: correction.replacement + separator)
            return remember(.corrected(correction, separator: separator), edit, on: text)
        }
        return remember(separator == " " ? .space : .other, Edit(insert: separator), on: text)
    }

    /// Right after a correction, backspace puts the typed word back and keeps it for the
    /// rest of the session; right after a swipe it deletes the whole word and its space;
    /// otherwise it deletes one character.
    mutating func backspace(before text: String, hostBehind: Bool = false) -> Edit {
        if hostBehind { return remember(.other, Edit(deleteCount: 1), on: text) }
        switch lastKey(before: text) {
        case let .corrected(correction, separator):
            lexicon.insert(correction.typed)
            let edit = Edit(deleteCount: (correction.replacement + separator).count, insert: correction.typed)
            return remember(.other, edit, on: text)
        case let .swiped(word, spaced, _):
            return remember(.other, Edit(deleteCount: word.count + (spaced ? 1 : 0)), on: text)
        case .other, .space, .chosen, .shift:
            return remember(.other, Edit(deleteCount: 1), on: text)
        }
    }

    /// A strip candidate replaces the word before the cursor and adds a space. Choosing the
    /// word as typed keeps it uncorrected for the rest of the session. After a swipe, the
    /// candidate swaps in for the swiped word and still goes in one backspace.
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

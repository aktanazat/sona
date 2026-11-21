import UIKit

/// A word replaced as it ended, kept so a backspace can put the typed form back.
struct Correction: Equatable {
    let typed: String
    let replacement: String
}

/// What the strip offers for the word being typed. `correction` is exactly what ending the
/// word would apply, so the strip never promises something space does not do. `restore` is
/// the typed form of a word corrected earlier that the cursor came back to; `emoji` stands
/// for the word. With nothing typed, `others` are the words likely to come next.
struct Suggestions: Equatable {
    let typed: String
    let correction: String?
    let others: [String]
    var restore: String?
    var emoji: String?
}

/// Words the user wants left as typed: Contacts names, text replacements, the Mac
/// vocabulary, words restored after a correction, and words the keyboard learned. Names,
/// vocabulary and learned words are also offered as the completion of a typed prefix,
/// spelled as they were given.
struct TypingLexicon {
    private var words: Set<String> = []
    private var expansions: [String: String] = [:]
    private(set) var names: [String] = []
    private(set) var learned: [String] = []
    private var learnedKeys: Set<String> = []

    init(replacements: [(shortcut: String, text: String)] = [], vocabulary: [String] = []) {
        replacements.forEach { add(shortcut: $0.shortcut, text: $0.text) }
        vocabulary.forEach { addName($0) }
    }

    /// A Contacts name or a text replacement: both sides stay as typed, and typing the
    /// shortcut offers its text. iOS hands Contacts names over with both sides the same.
    mutating func add(shortcut: String, text: String) {
        if shortcut == text { return addName(text) }
        insert(shortcut)
        insert(text)
        expansions[shortcut.lowercased()] = text
    }

    mutating func addName(_ phrase: String) {
        insert(phrase)
        for word in phrase.split(whereSeparator: { !TypingText.isWordCharacter($0) }).map(String.init)
        where !names.contains(word) {
            names.append(word)
        }
    }

    mutating func insert(_ phrase: String) {
        for word in phrase.split(whereSeparator: { !TypingText.isWordCharacter($0) }) {
            words.insert(word.lowercased())
        }
    }

    /// Replaces the learned words with `words`, as the keyboard's learning has them now.
    mutating func setLearned(_ words: [String]) {
        learned = words
        learnedKeys = Set(words.map { $0.lowercased() })
    }

    func contains(_ word: some StringProtocol) -> Bool {
        let key = word.lowercased()
        return words.contains(key) || learnedKeys.contains(key)
    }
    func expansion(of word: some StringProtocol) -> String? { expansions[word.lowercased()] }

    /// Names, vocabulary and learned words that start with `prefix`, ignoring case and accents.
    func names(startingWith prefix: some StringProtocol) -> [String] {
        guard prefix.count >= 2 else { return [] }
        let options: String.CompareOptions = [.caseInsensitive, .diacriticInsensitive, .anchored]
        return (names + learned).filter { $0.count > prefix.count && $0.range(of: prefix, options: options) != nil }
    }
}

/// Spelling correction for the word that ends at the cursor. Apple's on-device dictionary
/// does the spelling; this type only decides which guess, if any, is safe to apply.
@MainActor
final class TypingCorrector {
    struct Guess: Equatable {
        let word: String
        let score: Double
    }

    let language: String
    private let checker = UITextChecker()
    private let centres: [Character: (x: Double, y: Double)]
    /// The last word's guesses, so space reuses what the strip already worked out.
    private var lastGuesses: (typed: String, guesses: [Guess])?
    /// The language's common words, which complete words and rate how common a guess is:
    /// inside the keyboard extension Apple's checker completes nothing, in English as in
    /// Russian (it returned 0 completions for "t", "th" and "noon").
    var common: CommonWords? {
        didSet { lastGuesses = nil }
    }

    /// The closest spelling language to a keyboard language such as `en-US`.
    init?(language identifier: String) {
        let wanted = identifier.replacingOccurrences(of: "-", with: "_")
        let base = String(wanted.prefix { $0 != "_" })
        let available = UITextChecker.availableLanguages
        guard let match = available.first(where: { $0 == wanted })
            ?? available.first(where: { $0 == base || $0.hasPrefix(base + "_") })
        else { return nil }
        language = match
        centres = (TypingLanguage(rawValue: identifier) ?? .english).keyCentres
    }

    /// Has Apple's checker load its dictionary for the language, which its first check and
    /// first guesses otherwise do on the first space.
    func warmUp() {
        let sample = String(centres.keys.sorted().prefix(4))
        let range = NSRange(location: 0, length: sample.utf16.count)
        _ = checker.rangeOfMisspelledWord(in: sample, range: range, startingAt: 0, wrap: false, language: language)
        _ = checker.guesses(forWordRange: range, in: sample, language: language)
    }

    /// The replacement for the word that `text` ends with, or nil to keep it as typed.
    func correction(before text: String, lexicon: TypingLexicon) -> Correction? {
        guard let suggestions = suggestions(before: text, lexicon: lexicon),
              let replacement = suggestions.correction
        else { return nil }
        return Correction(typed: suggestions.typed, replacement: replacement)
    }

    /// What the strip offers for the word `text` ends with; nil when no word is being typed
    /// or nothing but the word as typed is on offer. Without `guessing`, only completions:
    /// Apple's guesses cost milliseconds on the main thread, where its checker must run, so
    /// the strip asks for them once typing pauses.
    func suggestions(before text: String, lexicon: TypingLexicon, guessing: Bool = true) -> Suggestions? {
        guard let word = TypingText.word(before: text) else { return nil }
        let offer = offer(for: word, after: text, lexicon: lexicon, guessing: guessing)
        return offer.correction == nil && offer.others.isEmpty ? nil : offer
    }

    private func offer(for word: Substring, after text: String, lexicon: TypingLexicon, guessing: Bool) -> Suggestions {
        let typed = String(word)
        let names = lexicon.names(startingWith: typed)
        let completions = names + completions(of: typed)
        if let expansion = lexicon.expansion(of: typed) {
            return Suggestions(typed: typed, correction: nil, others: Self.distinct([expansion] + completions, from: typed))
        }
        if language.hasPrefix("en"), !lexicon.contains(word), typed == "i" || typed.hasPrefix("i'") || typed.hasPrefix("i’") {
            return Suggestions(typed: typed, correction: "I" + typed.dropFirst(), others: Self.distinct(completions, from: typed))
        }
        guard guessing, needsCorrection(word, after: text, lexicon: lexicon) else {
            return Suggestions(typed: typed, correction: nil, others: Self.distinct(completions, from: typed))
        }
        let guesses = rankedGuesses(for: typed)
        let words = guesses.map { Self.matchCase(of: typed, in: $0.word) }
        /* A Russian word of three letters or fewer is too often chat ("ок", "спс", "кст")
         * that Apple's dictionary does not know: the strip offers a fix, and space leaves the
         * word alone. */
        let short = typed.count <= 3 && language.hasPrefix("ru")
        let clear = !short && (guesses.count < 2 || guesses[1].score - guesses[0].score >= Self.margin)
        return Suggestions(
            typed: typed,
            correction: clear ? words.first : nil,
            others: Self.distinct(names + words.dropFirst(clear ? 1 : 0) + completions, from: typed)
        )
    }

    /// Guesses closer than this to the runner-up are a coin toss, so the word stays as typed.
    static let margin = 0.025

    /* Apple's guesses come in dictionary order, not typing order: for "mettings" it offers
     * "nettings" before "meetings". They can also miss the obvious word ("превет" gets
     * "пресет" and "ревет" but not "привет"), so the most common words of about the same
     * length and the same first letters join them (at most 180), for words of four letters
     * or more: a short word has too many close neighbours ("ок" is one key from "он") for a
     * guess to be safe. Each guess is scored by how a thumb could have produced the typed
     * word from it plus how common it is, and only close guesses are kept. */
    func rankedGuesses(for typed: String) -> [Guess] {
        if let lastGuesses, lastGuesses.typed == typed { return lastGuesses.guesses }
        let letters = Array(typed.lowercased())
        let budget = letters.count <= 4 ? 1.0 : 2.0
        let apple = checker.guesses(
            forWordRange: NSRange(location: 0, length: typed.utf16.count), in: typed, language: language
        ) ?? []
        let known = Set(apple.map { $0.lowercased() } + [typed.lowercased()])
        let listed = letters.count < 4 ? [] : common?.candidates(near: typed.lowercased(), perBucket: 20) ?? []
        let extra = listed.filter { !known.contains($0) }
        let close = (apple + extra).enumerated().compactMap { rank, guess -> Guess? in
            guard !guess.contains(" "), !guess.contains("-") else { return nil }
            let cost = TypingText.slipCost(from: letters, to: Array(guess.lowercased()), centres: centres)
            guard cost <= budget else { return nil }
            return Guess(word: guess, score: cost + rarity(of: guess) + Double(min(rank, apple.count)) * 0.01)
        }
        let guesses = (close + splits(of: typed)).sorted { $0.score < $1.score }
        lastGuesses = (typed, guesses)
        return guesses
    }

    /// A missed space bar: two common words run together, such as "alot" or "inthe". The
    /// space costs what any missed key costs, so a close single word still wins. Both words
    /// must be among the language's 2,000 or so most common (rarity under 0.72).
    private func splits(of typed: String) -> [Guess] {
        typed.indices.dropFirst().compactMap { index in
            let first = String(typed[..<index]), second = String(typed[index...])
            let rarest = max(rarity(of: first), rarity(of: second))
            guard rarest < 0.72, !isMisspelled(first), !isMisspelled(second) else { return nil }
            return Guess(word: first + " " + second, score: 1 + rarest)
        }
    }

    /// A Russian word in the common words is one people write, such as "щас" or "прога",
    /// even where Apple's dictionary does not know it. English is left to Apple's
    /// dictionary: its common words also hold misspellings people often write, such as
    /// "occured" and "thats".
    private func needsCorrection(_ word: Substring, after text: String, lexicon: TypingLexicon) -> Bool {
        guard word.count > 1, !word.dropFirst().contains(where: \.isUppercase), !lexicon.contains(word),
              !TypingText.isStretched(word),
              !language.hasPrefix("ru") || common?.rarity(of: word.lowercased()) == nil
        else { return false }
        if word.first?.isUppercase == true, !TypingText.startsSentence(before: text[..<word.startIndex]) {
            return false
        }
        return isMisspelled(String(word))
    }

    private func isMisspelled(_ word: String) -> Bool {
        checker.rangeOfMisspelledWord(
            in: word, range: NSRange(location: 0, length: word.utf16.count),
            startingAt: 0, wrap: false, language: language
        ).location != NSNotFound
    }

    /// The language's common words that start with `word`, most common first, cased like it.
    func completions(of word: String) -> [String] {
        (common?.completions(of: word.lowercased(), limit: 20) ?? []).map { Self.matchCase(of: word, in: $0) }
    }

    /// 0 for the most common word, rising on a log scale to 1 for the least common of the
    /// common words; a word they lack, or any word before they load, is rarer than any.
    private func rarity(of word: String) -> Double {
        common?.rarity(of: word.lowercased()) ?? 1.3
    }

    private static func distinct(_ words: [String], from typed: String) -> [String] {
        var seen: Set<String> = [typed.lowercased()]
        return Array(words.filter { seen.insert($0.lowercased()).inserted }.prefix(3))
    }

    private static func matchCase(of typed: String, in guess: String) -> String {
        guard typed.first?.isUppercase == true else { return guess }
        return guess.prefix(1).uppercased() + guess.dropFirst()
    }
}

/// Reading words out of the text before the cursor.
enum TypingText {
    static func isWordCharacter(_ character: Character) -> Bool {
        character.isLetter || character == "'" || character == "’"
    }

    /// The plain word `text` ends with. Nil when the cursor is not right after a word, or the
    /// word belongs to an address, a number or code, which correction must not touch.
    static func word(before text: String) -> Substring? {
        let chunkStart = text.lastIndex(where: \.isWhitespace).map(text.index(after:)) ?? text.startIndex
        let chunk = text[chunkStart...]
        guard !chunk.contains(where: { $0.isNumber || "@/\\_#&=+<>|~".contains($0) }),
              !zip(chunk, chunk.dropFirst()).contains(where: { $0 == "." && $1.isLetter })
        else { return nil }
        let wordStart = chunk.lastIndex { !isWordCharacter($0) }.map(chunk.index(after:)) ?? chunk.startIndex
        let word = chunk[wordStart...].drop { $0 == "'" || $0 == "’" }
        guard word.last?.isLetter == true else { return nil }
        return word
    }

    /// A letter held three times or more, as in "sooo" or "yesss", is a word stretched on
    /// purpose; no dictionary word repeats a letter that often.
    static func isStretched(_ word: some StringProtocol) -> Bool {
        var run = 1
        for (previous, letter) in zip(word.lowercased(), word.lowercased().dropFirst()) {
            run = previous == letter ? run + 1 : 1
            if run >= 3 { return true }
        }
        return false
    }

    /// The whole words of the sentence before `text`'s end, last `limit` of them, oldest
    /// first; a word still being typed at the end is left out.
    static func sentenceWords(before text: Substring, limit: Int) -> [String] {
        let start = text.lastIndex { $0.isNewline || ".!?".contains($0) }.map(text.index(after:)) ?? text.startIndex
        var sentence = text[start...]
        if let last = sentence.last, isWordCharacter(last) {
            sentence = sentence[..<(sentence.lastIndex { !isWordCharacter($0) } ?? sentence.startIndex)]
        }
        let words = sentence.split { !isWordCharacter($0) }.map(String.init)
        return Array(words.suffix(limit))
    }

    /// Whether `text` leaves the cursor at the start of a sentence: nothing before it, a new
    /// line, or a space after a full stop, question mark or exclamation mark.
    static func startsSentence(before text: Substring) -> Bool {
        guard let last = text.last else { return true }
        if last.isNewline { return true }
        guard last == " " else { return false }
        guard let mark = text.last(where: { $0 != " " }) else { return true }
        return mark.isNewline || ".!?".contains(mark)
    }

    /* Costs of the slips a thumb makes on this layout. A neighbouring key, a swapped pair,
     * a doubled or dropped repeat, and a missing apostrophe cost less than an unrelated
     * letter, so among equally close guesses the likelier slip wins. */
    static func slipCost(
        from typed: [Character], to intended: [Character],
        centres: [Character: (x: Double, y: Double)] = englishCentres
    ) -> Double {
        var cost = Array(repeating: Array(repeating: 0.0, count: intended.count + 1), count: typed.count + 1)
        for i in typed.indices { cost[i + 1][0] = cost[i][0] + extraCost(typed, i, centres) }
        for j in intended.indices { cost[0][j + 1] = cost[0][j] + missingCost(intended, j) }
        for i in typed.indices {
            for j in intended.indices {
                var best = min(cost[i][j + 1] + extraCost(typed, i, centres), cost[i + 1][j] + missingCost(intended, j))
                best = min(best, cost[i][j] + substitutionCost(typed[i], intended[j], centres))
                if i > 0, j > 0, typed[i] == intended[j - 1], typed[i - 1] == intended[j] {
                    best = min(best, cost[i - 1][j - 1] + 0.6)
                }
                cost[i + 1][j + 1] = best
            }
        }
        return cost[typed.count][intended.count]
    }

    static let englishCentres = TypingLanguage.english.keyCentres

    private static func extraCost(
        _ typed: [Character], _ index: Int, _ centres: [Character: (x: Double, y: Double)]
    ) -> Double {
        let letter = typed[index]
        if index > 0, typed[index - 1] == letter { return 0.5 }
        let neighbours = [index - 1, index + 1].filter(typed.indices.contains).map { typed[$0] }
        return neighbours.contains { adjacent(letter, $0, centres) } ? 0.7 : 1
    }

    private static func missingCost(_ intended: [Character], _ index: Int) -> Double {
        let letter = intended[index]
        if letter == "'" || letter == "’" { return 0.3 }
        return index > 0 && intended[index - 1] == letter ? 0.5 : 1
    }

    private static func substitutionCost(
        _ typed: Character, _ intended: Character, _ centres: [Character: (x: Double, y: Double)]
    ) -> Double {
        if typed == intended { return 0 }
        return adjacent(typed, intended, centres) ? 0.6 : 1
    }

    private static func adjacent(
        _ first: Character, _ second: Character, _ centres: [Character: (x: Double, y: Double)]
    ) -> Bool {
        guard let a = centres[first], let b = centres[second] else { return false }
        let dx = a.x - b.x, dy = a.y - b.y
        return dx * dx + dy * dy < 2.1
    }
}

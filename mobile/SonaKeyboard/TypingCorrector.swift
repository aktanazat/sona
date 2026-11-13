import UIKit

/// A word replaced as it ended, kept so a backspace can put the typed form back.
struct Correction: Equatable {
    let typed: String
    let replacement: String
}

/// What the strip offers for the word being typed. `correction` is exactly what ending the
/// word would apply, so the strip never promises something space does not do.
struct Suggestions: Equatable {
    let typed: String
    let correction: String?
    let others: [String]
}

/// Words the user wants left as typed: Contacts names, text replacements, the Mac
/// vocabulary, and words restored after a correction.
struct TypingLexicon {
    private var words: Set<String> = []
    private var expansions: [String: String] = [:]

    init(replacements: [(shortcut: String, text: String)] = [], vocabulary: [String] = []) {
        replacements.forEach { add(shortcut: $0.shortcut, text: $0.text) }
        vocabulary.forEach { insert($0) }
    }

    /// A Contacts name or a text replacement: both sides stay as typed, and typing the
    /// shortcut offers its text.
    mutating func add(shortcut: String, text: String) {
        insert(shortcut)
        insert(text)
        if shortcut != text { expansions[shortcut.lowercased()] = text }
    }

    mutating func insert(_ phrase: String) {
        for word in phrase.split(whereSeparator: { !TypingText.isWordCharacter($0) }) {
            words.insert(word.lowercased())
        }
    }

    func contains(_ word: some StringProtocol) -> Bool { words.contains(word.lowercased()) }
    func expansion(of word: some StringProtocol) -> String? { expansions[word.lowercased()] }
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
    private var completionCache: [String: [String]] = [:]

    /// The closest spelling language to a keyboard language such as `en-US`.
    init?(language identifier: String) {
        let wanted = identifier.replacingOccurrences(of: "-", with: "_")
        let base = String(wanted.prefix { $0 != "_" })
        let available = UITextChecker.availableLanguages
        guard let match = available.first(where: { $0 == wanted })
            ?? available.first(where: { $0 == base || $0.hasPrefix(base + "_") })
        else { return nil }
        language = match
    }

    /// The replacement for the word that `text` ends with, or nil to keep it as typed.
    func correction(before text: String, lexicon: TypingLexicon) -> Correction? {
        guard let suggestions = suggestions(before: text, lexicon: lexicon),
              let replacement = suggestions.correction
        else { return nil }
        return Correction(typed: suggestions.typed, replacement: replacement)
    }

    /// What the strip offers for the word `text` ends with; nil when no word is being typed
    /// or nothing but the word as typed is on offer.
    func suggestions(before text: String, lexicon: TypingLexicon) -> Suggestions? {
        guard let word = TypingText.word(before: text) else { return nil }
        let offer = offer(for: word, after: text, lexicon: lexicon)
        return offer.correction == nil && offer.others.isEmpty ? nil : offer
    }

    private func offer(for word: Substring, after text: String, lexicon: TypingLexicon) -> Suggestions {
        let typed = String(word)
        let completions = completions(of: typed)
        if let expansion = lexicon.expansion(of: typed) {
            return Suggestions(typed: typed, correction: nil, others: Self.distinct([expansion] + completions, from: typed))
        }
        if language.hasPrefix("en"), !lexicon.contains(word), typed == "i" || typed.hasPrefix("i'") || typed.hasPrefix("i’") {
            return Suggestions(typed: typed, correction: "I" + typed.dropFirst(), others: Self.distinct(completions, from: typed))
        }
        guard needsCorrection(word, after: text, lexicon: lexicon) else {
            return Suggestions(typed: typed, correction: nil, others: Self.distinct(completions, from: typed))
        }
        let guesses = rankedGuesses(for: typed)
        let words = guesses.map { Self.matchCase(of: typed, in: $0.word) }
        let clear = guesses.count < 2 || guesses[1].score - guesses[0].score >= Self.margin
        return Suggestions(
            typed: typed,
            correction: clear ? words.first : nil,
            others: Self.distinct(Array(words.dropFirst(clear ? 1 : 0)) + completions, from: typed)
        )
    }

    /// Guesses closer than this to the runner-up are a coin toss, so the word stays as typed.
    static let margin = 0.025

    /* Apple's guesses come in dictionary order, not typing order: for "mettings" it offers
     * "nettings" before "meetings". Each guess is scored by how a thumb could have produced
     * the typed word from it plus how common it is, and only close guesses are kept. */
    func rankedGuesses(for typed: String) -> [Guess] {
        let letters = Array(typed.lowercased())
        let budget = letters.count <= 4 ? 1.0 : 2.0
        let guesses = checker.guesses(
            forWordRange: NSRange(location: 0, length: typed.utf16.count), in: typed, language: language
        ) ?? []
        let close = guesses.enumerated().compactMap { rank, guess -> Guess? in
            guard !guess.contains(" "), !guess.contains("-") else { return nil }
            let cost = TypingText.slipCost(from: letters, to: Array(guess.lowercased()))
            guard cost <= budget else { return nil }
            return Guess(word: guess, score: cost + rarity(of: guess) + Double(rank) * 0.01)
        }
        return (close + splits(of: typed)).sorted { $0.score < $1.score }
    }

    /// A missed space bar: two common words run together, such as "alot" or "inthe". The
    /// space costs what any missed key costs, so a close single word still wins.
    private func splits(of typed: String) -> [Guess] {
        typed.indices.dropFirst().compactMap { index in
            let first = String(typed[..<index]), second = String(typed[index...])
            let rarest = max(rarity(of: first), rarity(of: second))
            guard rarest < 0.5, !isMisspelled(first), !isMisspelled(second) else { return nil }
            return Guess(word: first + " " + second, score: 1 + rarest)
        }
    }

    private func needsCorrection(_ word: Substring, after text: String, lexicon: TypingLexicon) -> Bool {
        guard word.count > 1, !word.dropFirst().contains(where: \.isUppercase), !lexicon.contains(word) else {
            return false
        }
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

    private func completions(of word: String) -> [String] {
        if let known = completionCache[word] { return known }
        if completionCache.count > 500 { completionCache.removeAll() }
        let found = checker.completions(
            forPartialWordRange: NSRange(location: 0, length: word.utf16.count), in: word, language: language
        ) ?? []
        completionCache[word] = found
        return found
    }

    /// 0 for a word near the top of its first letter's completions, rising to 1 for one that
    /// even its first four letters never offer.
    private func rarity(of word: String) -> Double {
        let lower = word.lowercased()
        for length in 1...max(1, min(4, lower.count)) {
            let options = completions(of: String(lower.prefix(length)))
            if let rank = options.firstIndex(where: { $0.lowercased() == lower }) {
                return (Double(length - 1) + Double(rank) / Double(options.count)) / 4
            }
        }
        return 1
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
    static func slipCost(from typed: [Character], to intended: [Character]) -> Double {
        var cost = Array(repeating: Array(repeating: 0.0, count: intended.count + 1), count: typed.count + 1)
        for i in typed.indices { cost[i + 1][0] = cost[i][0] + extraCost(typed, i) }
        for j in intended.indices { cost[0][j + 1] = cost[0][j] + missingCost(intended, j) }
        for i in typed.indices {
            for j in intended.indices {
                var best = min(cost[i][j + 1] + extraCost(typed, i), cost[i + 1][j] + missingCost(intended, j))
                best = min(best, cost[i][j] + substitutionCost(typed[i], intended[j]))
                if i > 0, j > 0, typed[i] == intended[j - 1], typed[i - 1] == intended[j] {
                    best = min(best, cost[i - 1][j - 1] + 0.6)
                }
                cost[i + 1][j + 1] = best
            }
        }
        return cost[typed.count][intended.count]
    }

    private static func extraCost(_ typed: [Character], _ index: Int) -> Double {
        let letter = typed[index]
        if index > 0, typed[index - 1] == letter { return 0.5 }
        let neighbours = [index - 1, index + 1].filter(typed.indices.contains).map { typed[$0] }
        return neighbours.contains { adjacent(letter, $0) } ? 0.7 : 1
    }

    private static func missingCost(_ intended: [Character], _ index: Int) -> Double {
        let letter = intended[index]
        if letter == "'" || letter == "’" { return 0.3 }
        return index > 0 && intended[index - 1] == letter ? 0.5 : 1
    }

    private static func substitutionCost(_ typed: Character, _ intended: Character) -> Double {
        if typed == intended { return 0 }
        return adjacent(typed, intended) ? 0.6 : 1
    }

    private static let keyCentres: [Character: (x: Double, y: Double)] = {
        var centres: [Character: (x: Double, y: Double)] = [:]
        for (row, letters) in ["qwertyuiop", "asdfghjkl", "zxcvbnm"].enumerated() {
            let offset = [0.5, 1.0, 2.0][row]
            for (column, letter) in letters.enumerated() {
                centres[letter] = (offset + Double(column), Double(row))
            }
        }
        return centres
    }()

    private static func adjacent(_ first: Character, _ second: Character) -> Bool {
        guard let a = keyCentres[first], let b = keyCentres[second] else { return false }
        let dx = a.x - b.x, dy = a.y - b.y
        return dx * dx + dy * dy < 2.1
    }
}

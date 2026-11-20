import Foundation

/// The words most likely to come next, read from the bundled table for the language
/// (built by mobile/scripts/build-next-words.ts from Tatoeba's sentences). A context is the
/// last two words, the last word, or nothing at the start of a sentence.
final class NextWords: Sendable {
    private static let english = Task.detached(priority: .userInitiated) { bundled("en") }
    private static let russian = Task.detached(priority: .userInitiated) { bundled("ru") }

    private let table: [Substring: [Substring]]

    /// The bundled table for `language`, parsed once per process and off the main thread.
    static func table(for language: TypingLanguage) async -> NextWords? {
        switch language {
        case .english: await english.value
        case .russian: await russian.value
        }
    }

    private static func bundled(_ name: String) -> NextWords? {
        guard let url = Bundle(for: NextWords.self).url(forResource: name, withExtension: "tsv", subdirectory: "NextWords"),
              let contents = try? String(contentsOf: url, encoding: .utf8)
        else { return nil }
        return NextWords(contents: contents)
    }

    init(contents: String) {
        var table: [Substring: [Substring]] = [:]
        for line in contents.split(separator: "\n", omittingEmptySubsequences: true) {
            guard let tab = line.firstIndex(of: "\t") else { continue }
            table[line[..<tab]] = line[line.index(after: tab)...].split(separator: " ")
        }
        self.table = table
    }

    /// Up to `limit` words after `context`, the sentence's words before the cursor in order;
    /// the longest context the table knows decides first.
    func next(after context: [String], limit: Int) -> [String] {
        let keys = context.suffix(2).map { $0.lowercased() }
        var lookups: [String] = []
        if keys.count == 2 { lookups.append(keys.joined(separator: " ")) }
        if let last = keys.last { lookups.append(last) } else { lookups.append("") }
        var seen: Set<String> = []
        var found: [String] = []
        for key in lookups {
            for word in table[Substring(key)] ?? [] where seen.insert(word.lowercased()).inserted {
                found.append(String(word))
                if found.count == limit { return found }
            }
        }
        return found
    }
}

/// The language's common words, most common first, read from the bundled list (built by the
/// same script from the same sentences). They complete and correct words where the phone's
/// own dictionary offers nothing: on the simulator Apple's checker completes no Russian word
/// and its guesses for "превет" leave out "привет".
final class CommonWords: Sendable {
    private static let english = Task.detached(priority: .userInitiated) { bundled("en") }
    private static let russian = Task.detached(priority: .userInitiated) { bundled("ru") }

    /// Every word with its frequency rank and length, in alphabetical order for prefix lookups.
    private let sorted: [(word: String, rank: Int, length: Int)]

    /// The bundled list for `language`, read once per process and off the main thread.
    static func list(for language: TypingLanguage) async -> CommonWords? {
        switch language {
        case .english: await english.value
        case .russian: await russian.value
        }
    }

    private static func bundled(_ name: String) -> CommonWords? {
        guard let url = Bundle(for: CommonWords.self)
            .url(forResource: "\(name)-words", withExtension: "txt", subdirectory: "NextWords"),
            let contents = try? String(contentsOf: url, encoding: .utf8)
        else { return nil }
        return CommonWords(contents: contents)
    }

    init(contents: String) {
        sorted = contents.split(separator: "\n", omittingEmptySubsequences: true).enumerated()
            .map { (word: String($0.element), rank: $0.offset, length: $0.element.count) }
            .sorted { $0.word < $1.word }
    }

    /// Up to `limit` words that start with `prefix` and are longer than it, most common first.
    func completions(of prefix: String, limit: Int) -> [String] {
        sorted[firstIndex(notBefore: prefix)...].prefix { $0.word.hasPrefix(prefix) }
            .filter { $0.length > prefix.count }
            .sorted { $0.rank < $1.rank }
            .prefix(limit).map(\.word)
    }

    /// 0 for the most common word, rising on a log scale to 1 for the least common in the
    /// list; nil for a word the list does not have.
    func rarity(of word: String) -> Double? {
        let index = firstIndex(notBefore: word)
        guard index < sorted.count, sorted[index].word == word else { return nil }
        return log(Double(sorted[index].rank + 2)) / log(Double(sorted.count + 2))
    }

    private func firstIndex(notBefore word: String) -> Int {
        var low = 0, high = sorted.count
        while low < high {
            let middle = (low + high) / 2
            if sorted[middle].word < word { low = middle + 1 } else { high = middle }
        }
        return low
    }

    /// Up to `limit` words a thumb could have meant by `typed`, most common first: one letter
    /// longer or shorter at most, and starting with its first or second letter.
    func candidates(near typed: String, limit: Int) -> [String] {
        let letters = Array(typed)
        guard let first = letters.first else { return [] }
        let second = letters.dropFirst().first
        return sorted.filter { entry in
            abs(entry.length - letters.count) <= 1
                && (entry.word.first == first || entry.word.dropFirst().first == second)
        }
        .sorted { $0.rank < $1.rank }
        .prefix(limit).map(\.word)
    }
}

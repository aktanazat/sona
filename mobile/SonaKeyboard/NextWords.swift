import Foundation

/// The words most likely to come next, read from the bundled table for the language
/// (built by mobile/scripts/build-next-words.ts from Tatoeba's sentences). A context is the
/// last two words, the last word, or nothing at the start of a sentence.
final class NextWords: Sendable {
    private let table: [Substring: [Substring]]

    /// The bundled table for `language`, parsed off the main thread. Only the keyboard's
    /// current language is kept: switching languages lets the last one go.
    static func table(for language: TypingLanguage) async -> NextWords? {
        await Task.detached(priority: .userInitiated) {
            guard let url = Bundle(for: NextWords.self)
                .url(forResource: language.code.lowercased(), withExtension: "tsv", subdirectory: "NextWords"),
                let contents = try? String(contentsOf: url, encoding: .utf8)
            else { return nil }
            return NextWords(contents: contents)
        }.value
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
    /// Words of one length that start with `start`; a leading space stands for any first letter.
    private struct Bucket: Hashable {
        let start: String
        let length: Int
    }

    /// The words in lowercase, most common first; a word's index is its frequency rank.
    private let words: [String]
    /// How the few words not written in lowercase are written ("I", "I'm").
    private let display: [Int32: String]
    /// Ranks in alphabetical order of their words, for prefix lookups.
    private let alphabetical: [Int32]
    /// Ranks, most common first, by length and by first letter, first two letters, or second
    /// letter.
    private let buckets: [Bucket: [Int32]]

    /// The bundled list for `language`, read off the main thread. Only the keyboard's
    /// current language is kept: switching languages lets the last one go.
    static func list(for language: TypingLanguage) async -> CommonWords? {
        await Task.detached(priority: .userInitiated) {
            guard let url = Bundle(for: CommonWords.self)
                .url(forResource: "\(language.code.lowercased())-words", withExtension: "txt", subdirectory: "NextWords"),
                let contents = try? String(contentsOf: url, encoding: .utf8)
            else { return nil }
            return CommonWords(contents: contents)
        }.value
    }

    init(contents: String) {
        let written = contents.split(separator: "\n", omittingEmptySubsequences: true).map(String.init)
        let words = written.map { $0.lowercased() }
        var buckets: [Bucket: [Int32]] = [:], display: [Int32: String] = [:]
        for (rank, word) in words.enumerated() {
            if written[rank] != word { display[Int32(rank)] = written[rank] }
            for start in Self.starts(of: Array(word)) {
                buckets[Bucket(start: start, length: word.count), default: []].append(Int32(rank))
            }
        }
        self.words = words
        self.display = display
        alphabetical = words.indices.sorted { words[$0] < words[$1] }.map(Int32.init)
        self.buckets = buckets
    }

    private func shown(_ rank: Int32) -> String {
        display[rank] ?? words[Int(rank)]
    }

    /// The bucket starts a word is filed under: its first letter, its first two, and its
    /// second after a space.
    private static func starts(of letters: [Character]) -> [String] {
        guard let first = letters.first else { return [] }
        guard letters.count > 1 else { return [String(first)] }
        return [String(first), String(letters[0...1]), " " + String(letters[1])]
    }

    /// Up to `limit` words that start with `prefix` (in lowercase) and are longer than it,
    /// most common first.
    func completions(of prefix: String, limit: Int) -> [String] {
        alphabetical[firstIndex(notBefore: prefix)...].prefix { words[Int($0)].hasPrefix(prefix) }
            .filter { words[Int($0)] != prefix }
            .sorted()
            .prefix(limit).map(shown)
    }

    /// 0 for the most common word, rising on a log scale to 1 for the least common in the
    /// list; nil for a word the list does not have.
    func rarity(of word: String) -> Double? {
        let index = firstIndex(notBefore: word)
        guard index < alphabetical.count, words[Int(alphabetical[index])] == word else { return nil }
        return log(Double(alphabetical[index]) + 2) / log(Double(words.count + 2))
    }

    private func firstIndex(notBefore word: String) -> Int {
        var low = 0, high = alphabetical.count
        while low < high {
            let middle = (low + high) / 2
            if words[Int(alphabetical[middle])] < word { low = middle + 1 } else { high = middle }
        }
        return low
    }

    /// Words a thumb could have meant by `typed`, most common first: one letter longer or
    /// shorter at most, and starting with its first letter, its first two, or its second.
    /// Each bucket is already in frequency order, so only its top `perBucket` are taken: at
    /// most nine buckets, never the whole list.
    func candidates(near typed: String, perBucket: Int) -> [String] {
        let letters = Array(typed)
        guard !letters.isEmpty else { return [] }
        var picked: Set<Int32> = []
        for length in max(1, letters.count - 1)...(letters.count + 1) {
            for start in Self.starts(of: letters) {
                picked.formUnion(buckets[Bucket(start: start, length: length)]?.prefix(perBucket) ?? [])
            }
        }
        return picked.sorted().map(shown)
    }
}

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

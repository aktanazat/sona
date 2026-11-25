import Foundation

/// The emoji the strip offers for a typed word, with the English words that find each one
/// (built by mobile/scripts/build-emoji.ts from Unicode's emoji list and CLDR's annotations).
/// Picking emoji is the system emoji keyboard's job, so there is no panel here.
final class EmojiCatalog {
    @MainActor static let shared: EmojiCatalog? = {
        guard let url = Bundle(for: EmojiCatalog.self).url(forResource: "emoji", withExtension: "tsv"),
              let contents = try? String(contentsOf: url, encoding: .utf8)
        else { return nil }
        return EmojiCatalog(contents: contents)
    }()

    /// One emoji per single word: the emoji named exactly that word first, otherwise the
    /// first emoji in Apple's order that the word describes.
    private let byWord: [String: String]

    init(contents: String) {
        var all: [(glyph: String, name: String, keywords: [String])] = []
        for line in contents.split(separator: "\n") {
            let fields = line.split(separator: "\t", omittingEmptySubsequences: false)
            guard fields.count >= 5, Int(fields[0]) != nil else { continue }
            all.append((String(fields[1]), String(fields[2]), fields[3].split(separator: "|").map(String.init)))
        }
        var byWord: [String: String] = [:]
        for emoji in all where !emoji.name.contains(" ") {
            byWord[emoji.name] = byWord[emoji.name] ?? emoji.glyph
        }
        for emoji in all {
            for keyword in emoji.keywords where !keyword.contains(" ") && byWord[keyword] == nil {
                byWord[keyword] = emoji.glyph
            }
        }
        self.byWord = byWord
    }

    /// The emoji for a word typed in the text, when there is one.
    func emoji(for word: String) -> String? {
        guard word.count > 2 else { return nil }
        return byWord[word.lowercased()]
    }
}

import SwiftUI

/// Every emoji the keyboard offers, in Apple's order, with the English words that find it
/// (built by mobile/scripts/build-emoji.ts from Unicode's emoji list and CLDR's annotations).
final class EmojiCatalog {
    struct Emoji: Hashable {
        let glyph: String
        let group: Int
        let name: String
        let keywords: [String]
        /// The five skin-tone forms, light to dark; empty for an emoji without them.
        let tones: [String]
    }

    static let groups: [(symbol: String, label: LocalizedStringKey)] = [
        ("face.smiling", "keyboard.emoji.smileys"), ("figure.wave", "keyboard.emoji.people"),
        ("pawprint", "keyboard.emoji.nature"), ("fork.knife", "keyboard.emoji.food"),
        ("soccerball", "keyboard.emoji.activities"), ("car", "keyboard.emoji.travel"),
        ("lightbulb", "keyboard.emoji.objects"), ("music.note", "keyboard.emoji.symbols"),
        ("flag", "keyboard.emoji.flags"),
    ]

    @MainActor static let shared: EmojiCatalog? = {
        guard let url = Bundle(for: EmojiCatalog.self).url(forResource: "emoji", withExtension: "tsv"),
              let contents = try? String(contentsOf: url, encoding: .utf8)
        else { return nil }
        return EmojiCatalog(contents: contents)
    }()

    let all: [Emoji]
    /// `all` split by group, in the order of `groups`.
    let grouped: [[Emoji]]
    private let byGlyph: [String: Emoji]
    /// One emoji per single word, for the strip: the emoji named exactly that word first,
    /// otherwise the first emoji in Apple's order that the word describes.
    private let byWord: [String: String]

    init(contents: String) {
        var all: [Emoji] = []
        for line in contents.split(separator: "\n") {
            let fields = line.split(separator: "\t", omittingEmptySubsequences: false)
            guard fields.count >= 5, let group = Int(fields[0]) else { continue }
            all.append(Emoji(
                glyph: String(fields[1]), group: group, name: String(fields[2]),
                keywords: fields[3].split(separator: "|").map(String.init),
                tones: fields[4].split(separator: " ").map(String.init)
            ))
        }
        self.all = all
        grouped = Self.groups.indices.map { index in all.filter { $0.group == index } }
        var byGlyph: [String: Emoji] = [:]
        var byWord: [String: String] = [:]
        for emoji in all {
            byGlyph[emoji.glyph] = emoji
            emoji.tones.forEach { byGlyph[$0] = emoji }
            if !emoji.name.contains(" ") { byWord[emoji.name] = byWord[emoji.name] ?? emoji.glyph }
        }
        for emoji in all {
            for keyword in emoji.keywords where !keyword.contains(" ") && byWord[keyword] == nil {
                byWord[keyword] = emoji.glyph
            }
        }
        self.byGlyph = byGlyph
        self.byWord = byWord
    }

    func entry(for glyph: String) -> Emoji? { byGlyph[glyph] }

    /// The emoji for a word typed in the text, when there is one.
    func emoji(for word: String) -> String? {
        guard word.count > 2 else { return nil }
        return byWord[word.lowercased()]
    }

    /// Emoji whose words start with every word of `query`. The emoji named exactly by the
    /// query come first, then those whose name holds it, plainest name first ("red heart"
    /// before "heart with arrow"), then those it describes, each in Apple's order.
    func search(_ query: String, limit: Int = 60) -> [Emoji] {
        let terms = query.lowercased().split(separator: " ").map(String.init)
        guard !terms.isEmpty else { return [] }
        let matching = all.filter { emoji in
            terms.allSatisfy { term in
                emoji.keywords.contains { keyword in keyword.split(separator: " ").contains { $0.hasPrefix(term) } }
            }
        }
        let exact = terms.joined(separator: " ")
        func rank(_ emoji: Emoji) -> (Int, Int) {
            if emoji.name == exact { return (0, 0) }
            if " \(emoji.name) ".contains(" \(exact) ") { return (1, emoji.name.count) }
            return (emoji.keywords.contains(exact) ? 2 : 3, 0)
        }
        let ranked = matching.enumerated().sorted { lhs, rhs in
            let (left, right) = (rank(lhs.element), rank(rhs.element))
            return left != right ? left < right : lhs.offset < rhs.offset
        }
        return ranked.prefix(limit).map(\.element)
    }
}

/// The emoji used lately and the skin tone chosen for each emoji, kept in the keyboard's own
/// defaults so they work without Full Access. The tones are read once, as a page of emoji
/// asks for about two thousand of them.
struct EmojiMemory {
    static let recentsKey = "sona.keyboard.emoji.recents"
    static let tonesKey = "sona.keyboard.emoji.tones"
    static let recentLimit = 32
    let defaults: UserDefaults
    private var tones: [String: String]

    init(defaults: UserDefaults) {
        self.defaults = defaults
        tones = defaults.dictionary(forKey: Self.tonesKey) as? [String: String] ?? [:]
    }

    var recents: [String] { defaults.stringArray(forKey: Self.recentsKey) ?? [] }

    func used(_ glyph: String) {
        let list = [glyph] + recents.filter { $0 != glyph }
        defaults.set(Array(list.prefix(Self.recentLimit)), forKey: Self.recentsKey)
    }

    /// The form of `base` last chosen, or `base` itself.
    func toned(_ base: String) -> String {
        tones[base] ?? base
    }

    mutating func choose(_ form: String, for base: String) {
        tones[base] = form
        defaults.set(tones, forKey: Self.tonesKey)
    }
}


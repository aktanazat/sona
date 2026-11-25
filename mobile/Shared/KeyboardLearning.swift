import Foundation

/// The words the keyboard learned from the person's own typing, and the word that follows
/// each of them most often. Nothing leaves the phone: it lives in the container the app and
/// keyboard share, and Reset in the app empties it.
struct KeyboardLearning: Codable, Equatable {
    static let key = "sona.keyboard.learning"
    /// A word typed this many times, uncorrected, is one the person means.
    static let knownAfter = 2
    private static let wordLimit = 4000
    private static let followLimit = 4000

    private(set) var words: [String: Int] = [:]
    private(set) var follows: [String: [String: Int]] = [:]
    /// Whether learned words were once checked for typos learned by mistake; nil in learning
    /// saved before that check existed.
    var typosChecked: Bool?

    /// Learned words, spelled as the person types them in mid-sentence.
    var known: [String] { words.filter { $0.value >= Self.knownAfter }.map(\.key) }

    func knows(_ word: String) -> Bool { (words[word] ?? 0) >= Self.knownAfter }

    /// A word ended with space or punctuation, and the word before it in the same sentence.
    /// A word that opens a sentence comes with its first letter lowered unless it is a name.
    mutating func typed(_ word: String, after previous: String?) {
        words[word, default: 0] += 1
        if let previous {
            follows[previous.lowercased(), default: [:]][word, default: 0] += 1
        }
        prune()
    }

    /// A word kept on purpose, by undoing a correction or choosing it as typed, is known at once.
    mutating func keep(_ word: String) {
        words[word] = max(words[word] ?? 0, Self.knownAfter)
        prune()
    }

    /// Forgets `forgotten` as words and as what follows another word.
    mutating func forget(_ forgotten: [String]) {
        for word in forgotten {
            words[word] = nil
            for key in follows.keys { follows[key]?[word] = nil }
            follows[word.lowercased()] = nil
        }
    }

    /// What the person most often typed after `word`, most often first.
    func next(after word: String, limit: Int) -> [String] {
        guard let counts = follows[word.lowercased()] else { return [] }
        return counts.filter { $0.value >= Self.knownAfter }
            .sorted { $0.value > $1.value || ($0.value == $1.value && $0.key < $1.key) }
            .prefix(limit).map(\.key)
    }

    /// Keeps the store small: the rarest entries go first once a table is over its limit.
    private mutating func prune() {
        if words.count > Self.wordLimit {
            let keep = words.sorted { $0.value > $1.value }.prefix(Self.wordLimit * 3 / 4)
            words = Dictionary(uniqueKeysWithValues: keep.map { ($0.key, $0.value) })
        }
        if follows.count > Self.followLimit {
            let keep = follows.sorted { $0.value.values.reduce(0, +) > $1.value.values.reduce(0, +) }
                .prefix(Self.followLimit * 3 / 4)
            follows = Dictionary(uniqueKeysWithValues: keep.map { ($0.key, $0.value) })
        }
    }
}

/// Reads and writes `KeyboardLearning` in the shared container.
struct KeyboardLearningStore {
    private static let resetsKey = "sona.keyboard.learning.resets"
    let defaults: UserDefaults

    static func shared() -> Self {
        Self(defaults: UserDefaults(suiteName: KeyboardDraftStore.group) ?? .standard)
    }

    /// The stored learning, and how many times Reset had run when it was read.
    func load() -> (learning: KeyboardLearning, resets: Int) {
        let resets = defaults.integer(forKey: Self.resetsKey)
        guard let data = defaults.data(forKey: KeyboardLearning.key),
              let learning = try? JSONDecoder().decode(KeyboardLearning.self, from: data)
        else { return (KeyboardLearning(), resets) }
        return (learning, resets)
    }

    /// Writes `learning`, which grew from a load when Reset had run `resets` times. When
    /// Reset ran since, the reset wins: nothing is written, and the result is false.
    func save(_ learning: KeyboardLearning, readAt resets: Int) throws -> Bool {
        guard defaults.integer(forKey: Self.resetsKey) == resets else { return false }
        defaults.set(try JSONEncoder().encode(learning), forKey: KeyboardLearning.key)
        return true
    }

    func reset() {
        defaults.removeObject(forKey: KeyboardLearning.key)
        defaults.set(defaults.integer(forKey: Self.resetsKey) + 1, forKey: Self.resetsKey)
    }
}

import CoreGraphics
import Foundation

/// The words a finger's path across the letter keys most likely spelled. A word's shape is
/// the line through its letters' key centres; the path is compared with it point by point
/// after both are resampled to the same number of evenly spaced points.
struct SwipeDecoder {
    static let samples = 32

    let language: TypingLanguage
    let centres: [Character: CGPoint]
    let keyWidth: CGFloat

    /// Up to four words, best first. `dictionary` returns the words that start with a prefix,
    /// most common first; `names` are offered whenever their shape fits.
    func words(for path: [CGPoint], dictionary: (String) -> [String], names: [String]) -> [String] {
        let length = Self.length(of: path)
        guard path.count > 1, length >= keyWidth else { return [] }
        let points = Self.resample(path, count: Self.samples)
        let dense = Self.resample(path, count: max(Self.samples, Int(length / (keyWidth / 4))))
        let firsts = Array(nearest(to: points[0], within: keyWidth * 1.1).prefix(3))
        let lasts = Set(nearest(to: points[points.count - 1], within: keyWidth * 1.1))
        let passed = Set(dense.flatMap { nearest(to: $0, within: keyWidth * 0.8) })

        var seen: Set<String> = []
        var scored: [(word: String, score: CGFloat)] = []
        func consider(_ word: String, rarity: CGFloat) {
            guard seen.insert(word.lowercased()).inserted else { return }
            let letters = keys(of: word)
            guard letters.count > 1, let first = letters.first, let last = letters.last,
                  firsts.contains(first), lasts.contains(last), letters.allSatisfy(passed.contains)
            else { return }
            let shape = Self.resample(letters.compactMap { centres[$0] }, count: Self.samples)
            let distance = zip(points, shape).reduce(0) { $0 + hypot($1.0.x - $1.1.x, $1.0.y - $1.1.y) }
            scored.append((word, distance / CGFloat(Self.samples) / keyWidth + 0.3 * rarity))
        }
        for prefix in prefixes(firsts: firsts, passed: passed) {
            let found = dictionary(prefix)
            for (rank, word) in found.enumerated() where !word.contains(" ") {
                consider(word, rarity: CGFloat(rank) / CGFloat(max(found.count, 1)))
            }
        }
        for name in names { consider(name, rarity: 0) }
        return scored.sorted { $0.score < $1.score }.prefix(4).map(\.word)
    }

    /// Two-letter prefixes from a starting key and a key the path crossed, with the
    /// language's accented forms for languages that use them natively, so "école" is found
    /// from a path that starts on e.
    private func prefixes(firsts: [Character], passed: Set<Character>) -> [String] {
        let expandAccents = language != .english
        let variants = { (letter: Character) in
            guard expandAccents, let accents = language.accents[letter] else { return [String(letter)] }
            return [String(letter)] + accents.map(String.init)
        }
        var seen: Set<String> = []
        var found: [String] = []
        for first in firsts {
            for start in variants(first) {
                for second in passed.union([first]).flatMap(variants) {
                    let prefix = start + second
                    if seen.insert(prefix).inserted { found.append(prefix) }
                }
            }
        }
        return found
    }

    /// The keys a word's letters sit on, an accented letter on its plain key and a doubled
    /// letter once, since a path cannot show a key twice in a row.
    private func keys(of word: String) -> [Character] {
        var keys: [Character] = []
        for character in word.lowercased() {
            let key = centres[character] != nil ? character
                : String(character).folding(options: .diacriticInsensitive, locale: nil).first
            guard let key, centres[key] != nil, keys.last != key else { continue }
            keys.append(key)
        }
        return keys
    }

    private func nearest(to point: CGPoint, within radius: CGFloat) -> [Character] {
        centres.map { (letter: $0.key, distance: hypot($0.value.x - point.x, $0.value.y - point.y)) }
            .filter { $0.distance <= radius }
            .sorted { $0.distance < $1.distance }
            .map(\.letter)
    }

    static func length(of path: [CGPoint]) -> CGFloat {
        zip(path, path.dropFirst()).reduce(0) { $0 + hypot($1.1.x - $1.0.x, $1.1.y - $1.0.y) }
    }

    /// `count` points evenly spaced along the path; a single point repeated when it has no length.
    static func resample(_ path: [CGPoint], count: Int) -> [CGPoint] {
        guard let start = path.first else { return [] }
        let total = length(of: path)
        guard total > 0, count > 1 else { return Array(repeating: start, count: count) }
        let step = total / CGFloat(count - 1)
        var result = [start]
        var travelled: CGFloat = 0
        var target = step
        for (a, b) in zip(path, path.dropFirst()) {
            let segment = hypot(b.x - a.x, b.y - a.y)
            while segment > 0, target <= travelled + segment, result.count < count - 1 {
                let t = (target - travelled) / segment
                result.append(CGPoint(x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t))
                target += step
            }
            travelled += segment
        }
        while result.count < count { result.append(path[path.count - 1]) }
        return result
    }
}

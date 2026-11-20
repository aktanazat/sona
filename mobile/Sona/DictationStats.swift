import Foundation

/// What this iPhone has dictated since the stats were last reset.
struct DictationStats: Codable, Equatable {
    var dictations = 0
    var words = 0
    var seconds: Double = 0
    var since = Date()

    /// Words per minute of speaking, or nil before anything was dictated.
    var wordsPerMinute: Int? {
        seconds > 0 ? Int((Double(words) / (seconds / 60)).rounded()) : nil
    }

    mutating func record(_ text: String, seconds: TimeInterval) {
        let count = text.split(whereSeparator: { $0.isWhitespace || $0.isNewline }).count
        guard count > 0 else { return }
        dictations += 1
        words += count
        self.seconds += max(seconds, 0)
    }

    private static let key = "sona.dictation.stats"

    static func load(_ defaults: UserDefaults = .standard) -> DictationStats {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(DictationStats.self, from: $0) } ?? DictationStats()
    }

    func save(_ defaults: UserDefaults = .standard) {
        if let data = try? JSONEncoder().encode(self) { defaults.set(data, forKey: Self.key) }
    }
}

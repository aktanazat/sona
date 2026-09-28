import Foundation

/// The Mac publishes this inside a normal encrypted companion object. No credential is included.
struct DictationProfile: Codable, Equatable {
    static let sourceFormat = "sona-dictation-profile-v1"
    struct Word: Codable, Equatable, Hashable {
        var spoken: String
        var written: String
    }
    struct Snippet: Codable, Equatable, Hashable {
        var trigger: String
        var expansion: String
    }
    struct Style: Codable, Equatable, Identifiable {
        var id: String
        var name: String
        var prompt: String
    }
    var version: Int
    var vocabulary: [Word]
    var replacements: [Word]
    var snippets: [Snippet]
    var styles: [Style]

    static let empty = Self(version: 1, vocabulary: [], replacements: [], snippets: [], styles: [])

    var recognitionHints: [String] {
        // Apple recommends no more than 100 contextual phrases. Replacements still use every entry.
        Array(Set(vocabulary.map(\.written) + snippets.map(\.trigger)).sorted().prefix(100))
    }

    func apply(to text: String) -> String {
        let replaced = Self.replace(text, pairs: replacements.map { ($0.spoken, $0.written) })
        let corrected = Self.replace(replaced, pairs: vocabulary.map { ($0.spoken, $0.written) })
        return Self.replace(corrected, pairs: snippets.map { ($0.trigger, $0.expansion) })
    }

    /// Longest whole-token match wins. Inserted text is never scanned again.
    private static func replace(_ text: String, pairs: [(String, String)]) -> String {
        let pairs = pairs.filter { !$0.0.isEmpty && !$0.1.isEmpty }
            .map { (spoken: $0.0.lowercased(), length: $0.0.count, written: $0.1) }
            .sorted { $0.length > $1.length }
        guard !pairs.isEmpty else { return text }
        var output = ""
        output.reserveCapacity(text.utf8.count)
        var cursor = text.startIndex
        func isToken(_ character: Character) -> Bool {
            character.isLetter || character.isNumber || character == "_"
        }
        while cursor < text.endIndex {
            let boundary = cursor == text.startIndex || !isToken(text[text.index(before: cursor)])
            var matched = false
            if boundary {
                for (spoken, length, written) in pairs {
                    guard let end = text.index(cursor, offsetBy: length, limitedBy: text.endIndex),
                          text[cursor..<end].lowercased() == spoken,
                          end == text.endIndex || !isToken(text[end])
                    else { continue }
                    output += written
                    cursor = end
                    matched = true
                    break
                }
            }
            if !matched {
                output.append(text[cursor])
                cursor = text.index(after: cursor)
            }
        }
        return output
    }
}

/// A persisted language list, plus the active language Apple Speech needs for a session.
struct DictationPreferences {
    static let keyboardKey = "sona.keyboard.enabled"
    static let minutesKey = "sona.keyboard.minutes"
    static let languagesKey = "sona.dictation.languages"
    static let activeLanguageKey = "sona.dictation.activeLanguage"
    static let styleKey = "sona.dictation.style"

    static var languages: [String] {
        get { UserDefaults.standard.stringArray(forKey: languagesKey) ?? [Locale.current.identifier] }
        set {
            guard !newValue.isEmpty else { return }
            UserDefaults.standard.set(newValue, forKey: languagesKey)
            if !newValue.contains(activeLanguage) { activeLanguage = newValue[0] }
        }
    }
    static var activeLanguage: String {
        get { UserDefaults.standard.string(forKey: activeLanguageKey) ?? languages.first ?? "en-US" }
        set { UserDefaults.standard.set(newValue, forKey: activeLanguageKey) }
    }
    static var warmMinutes: Int {
        let stored = UserDefaults.standard.integer(forKey: minutesKey)
        return [1, 5, 15, 30].contains(stored) ? stored : 5
    }
}

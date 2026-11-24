import Foundation

/// A language the Sona keyboard types in. The raw value is the locale spelling, next words
/// and dictation use.
enum TypingLanguage: String, CaseIterable, Identifiable, Sendable {
    case english = "en-US"
    case russian = "ru-RU"

    var id: String { rawValue }

    /// The letter rows as the system keyboard for the language lays them out.
    var letterRows: [String] {
        switch self {
        case .english: ["qwertyuiop", "asdfghjkl", "zxcvbnm"]
        case .russian: ["йцукенгшщзх", "фывапролджэ", "ячсмитьбю"]
        }
    }

    /// The accented letters a held key offers, most used first.
    var accents: [Character: String] {
        switch self {
        case .english: ["e": "éèêë", "a": "àáâä", "o": "óôö", "u": "úü", "i": "íï", "n": "ñ", "c": "ç"]
        case .russian: ["е": "ё", "ь": "ъ"]
        }
    }

    /// The language key's face.
    var code: String {
        switch self {
        case .english: "EN"
        case .russian: "RU"
        }
    }

    /// The language's own name for itself, which needs no translation.
    var name: String {
        switch self {
        case .english: "English"
        case .russian: "Русский"
        }
    }

    /// Key widths across the layout: the longest of the first two rows, at least ten. Shift
    /// and delete share what the third row leaves, so every row spans the same width.
    var columns: Int {
        max(10, letterRows[0].count, letterRows[1].count)
    }

    /// Letter centres in key widths across and rows down, laid out like the keyboard draws
    /// them: every row centred, the third between shift and delete.
    var keyCentres: [Character: (x: Double, y: Double)] {
        var centres: [Character: (x: Double, y: Double)] = [:]
        for (row, letters) in letterRows.map(Array.init).enumerated() {
            let start = Double(columns - letters.count) / 2
            for (column, letter) in letters.enumerated() {
                centres[letter] = (start + Double(column) + 0.5, Double(row))
            }
        }
        return centres
    }
}

struct KeyboardLanguages {
    static let enabledKey = "sona.keyboard.languages"
    let defaults: UserDefaults

    static func shared() -> Self {
        Self(defaults: UserDefaults(suiteName: KeyboardDraftStore.group) ?? .standard)
    }

    /// A stored language the keyboard no longer has, such as one from an earlier version,
    /// counts as English.
    var enabled: [TypingLanguage] {
        get {
            let stored = Set((defaults.stringArray(forKey: Self.enabledKey) ?? []).map { TypingLanguage(rawValue: $0) ?? .english })
            return stored.isEmpty ? [.english] : TypingLanguage.allCases.filter(stored.contains)
        }
        nonmutating set {
            guard !newValue.isEmpty else { return }
            defaults.set(TypingLanguage.allCases.filter(newValue.contains).map(\.rawValue), forKey: Self.enabledKey)
        }
    }

    /// The language after `language` in the enabled list, wrapping around.
    func next(after language: TypingLanguage) -> TypingLanguage {
        let list = enabled
        guard let index = list.firstIndex(of: language) else { return list[0] }
        return list[(index + 1) % list.count]
    }
}

/// How the keyboard looks and answers a key, chosen in the app and read by the keyboard
/// through the shared container.
struct KeyboardFeelPreferences {
    static let numberRowKey = "sona.keyboard.numberRow"
    static let hapticsKey = "sona.keyboard.haptics"
    static let soundsKey = "sona.keyboard.sounds"
    let defaults: UserDefaults

    static func shared() -> Self {
        Self(defaults: UserDefaults(suiteName: KeyboardDraftStore.group) ?? .standard)
    }

    var numberRow: Bool { defaults.bool(forKey: Self.numberRowKey) }
    var haptics: Bool { defaults.object(forKey: Self.hapticsKey) as? Bool ?? true }
    var sounds: Bool { defaults.object(forKey: Self.soundsKey) as? Bool ?? true }
}

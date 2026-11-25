import Foundation

/// A language the Sona keyboard types in. The raw value is the locale spelling, next words
/// and dictation use.
enum TypingLanguage: String, CaseIterable, Identifiable, Sendable {
    case english = "en-US"
    case russian = "ru-RU"

    var id: String { rawValue }

    func letterGrid(englishLayout: EnglishKeyboardLayout = .qwerty) -> KeyboardLetterGrid {
        switch self {
        case .english: englishLayout.grid
        case .russian: .russian
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
}

enum EnglishKeyboardLayout: String, CaseIterable, Sendable {
    case qwerty
    case qwertz

    var grid: KeyboardLetterGrid {
        switch self {
        case .qwerty: .qwerty
        case .qwertz: .qwertz
        }
    }
}

struct KeyboardLetterGrid: Sendable {
    static let qwerty = Self(["qwertyuiop", "asdfghjkl", "zxcvbnm"])
    static let qwertz = Self(["qwertzuiop", "asdfghjkl", "yxcvbnm"])
    static let russian = Self(["йцукенгшщзх", "фывапролджэ", "ячсмитьбю"])

    let rows: [String]
    let columns: Int
    let keyCentres: [Character: (x: Double, y: Double)]

    private init(_ rows: [String]) {
        self.rows = rows
        columns = max(10, rows[0].count, rows[1].count)
        var centres: [Character: (x: Double, y: Double)] = [:]
        for (row, letters) in rows.enumerated() {
            let start = Double(columns - letters.count) / 2
            for (column, letter) in letters.enumerated() {
                centres[letter] = (start + Double(column) + 0.5, Double(row))
            }
        }
        keyCentres = centres
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
    static let englishLayoutKey = "sona.keyboard.englishLayout"
    static let numberRowKey = "sona.keyboard.numberRow"
    static let hapticsKey = "sona.keyboard.haptics"
    static let soundsKey = "sona.keyboard.sounds"
    let defaults: UserDefaults

    static func shared() -> Self {
        Self(defaults: UserDefaults(suiteName: KeyboardDraftStore.group) ?? .standard)
    }

    var englishLayout: EnglishKeyboardLayout {
        defaults.string(forKey: Self.englishLayoutKey).flatMap(EnglishKeyboardLayout.init(rawValue:)) ?? .qwerty
    }
    var numberRow: Bool { defaults.bool(forKey: Self.numberRowKey) }
    var haptics: Bool { defaults.object(forKey: Self.hapticsKey) as? Bool ?? true }
    var sounds: Bool { defaults.object(forKey: Self.soundsKey) as? Bool ?? true }
}

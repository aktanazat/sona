import Foundation

/// The languages the Sona keyboard types in. The raw value is the locale spelling and
/// dictation use.
enum TypingLanguage: String, CaseIterable, Identifiable {
    case english = "en-US"

    var id: String { rawValue }

    /// The language key's face.
    var code: String { String(rawValue.prefix(2)).uppercased() }

    /// The language's own name for itself, which needs no translation.
    var name: String {
        let locale = Locale(identifier: rawValue)
        let name = locale.localizedString(forLanguageCode: String(rawValue.prefix(2))) ?? rawValue
        return name.prefix(1).uppercased(with: locale) + name.dropFirst()
    }

    /// The letter rows as the system keyboard for that language lays them out.
    var letterRows: [String] {
        switch self {
        case .english: ["qwertyuiop", "asdfghjkl", "zxcvbnm"]
        }
    }

    /// The accented letters a held key offers, most used first.
    var accents: [Character: String] {
        switch self {
        case .english: ["e": "éèêë", "a": "àáâä", "o": "óôö", "u": "úü", "i": "íï", "n": "ñ", "c": "ç"]
        }
    }

    /// Letter centres in key widths across and rows down, laid out like the keyboard draws
    /// them: rows centred in the widest row, the third between shift and delete.
    var keyCentres: [Character: (x: Double, y: Double)] {
        let rows = letterRows.map(Array.init)
        let width = Double(max(10, rows[0].count, rows[1].count, rows[2].count + 3))
        var centres: [Character: (x: Double, y: Double)] = [:]
        for (row, letters) in rows.enumerated() {
            let used = Double(letters.count) + (row == 2 ? 3 : 0)
            let start = (width - used) / 2 + (row == 2 ? 1.5 : 0)
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

    var enabled: [TypingLanguage] {
        get {
            let stored = (defaults.stringArray(forKey: Self.enabledKey) ?? []).compactMap(TypingLanguage.init)
            return stored.isEmpty ? [.english] : stored
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

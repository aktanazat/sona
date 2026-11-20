import Foundation
import SwiftUI

/// Case changes the keyboard applies to a field's text in place, with no model.
enum TextCase: String, CaseIterable, Identifiable {
    case upper, lower, title, sentence

    var id: String { rawValue }

    var label: LocalizedStringKey {
        switch self {
        case .upper: "tools.case.upper"
        case .lower: "tools.case.lower"
        case .title: "tools.case.title"
        case .sentence: "tools.case.sentence"
        }
    }

    func apply(to text: String, locale: Locale = .current) -> String {
        switch self {
        case .upper: text.uppercased(with: locale)
        case .lower: text.lowercased(with: locale)
        case .title: Self.title(of: text, locale: locale)
        case .sentence: Self.sentences(of: text.lowercased(with: locale), locale: locale)
        }
    }

    /// Each run of letters transformed, everything between them kept as it was.
    private static func words(of text: String, _ transform: (String) -> String) -> String {
        var output = ""
        var word = ""
        for character in text {
            if character.isLetter || character == "'" || character == "’" {
                word.append(character)
            } else {
                if !word.isEmpty { output += transform(word); word = "" }
                output.append(character)
            }
        }
        if !word.isEmpty { output += transform(word) }
        return output
    }

    /// English titles keep articles, short conjunctions and short prepositions lowercase
    /// between the first and last word.
    private static let minorWords: Set<String> = ["a", "an", "and", "as", "at", "but", "by", "for", "in", "nor", "of", "on", "or", "the", "to"]

    private static func title(of text: String, locale: Locale) -> String {
        var count = 0
        _ = words(of: text) { count += 1; return $0 }
        let english = locale.language.languageCode == .english
        var index = 0
        return words(of: text) { word in
            defer { index += 1 }
            let lower = word.lowercased(with: locale)
            if english, index > 0, index < count - 1, minorWords.contains(lower) { return lower }
            return lower.prefix(1).uppercased(with: locale) + lower.dropFirst()
        }
    }

    /// A capital for the first letter of the text and of every sentence, and for "I".
    private static func sentences(of text: String, locale: Locale) -> String {
        var output = ""
        var capital = true
        for character in text {
            if capital, character.isLetter {
                output += String(character).uppercased(with: locale)
                capital = false
            } else {
                output.append(character)
            }
            if ".!?\n".contains(character) { capital = true }
        }
        guard locale.language.languageCode == .english || locale.identifier.hasPrefix("en") else { return output }
        return words(of: output) { $0 == "i" || $0.hasPrefix("i'") || $0.hasPrefix("i’") ? "I" + $0.dropFirst() : String($0) }
    }
}

/// Raycast's text styles: Latin letters and digits swapped for Unicode look-alikes, which
/// show the style in fields that take no formatting.
enum TextStyle: String, CaseIterable, Identifiable {
    case bold, italic, boldItalic, script, monospace, doubleStruck, circled, strikethrough, underline

    var id: String { rawValue }

    /// The style's name written in the style, so the chip shows what a tap does.
    var sample: String { apply(to: NSLocalizedString("tools.style.\(rawValue)", comment: "")) }

    var label: LocalizedStringKey { LocalizedStringKey("tools.style.\(rawValue)") }

    /// First scalar of "A", "a" and "0" in the style; nil leaves that kind of character.
    private var bases: (upper: UInt32, lower: UInt32, digit: UInt32?)? {
        switch self {
        case .bold: (0x1D400, 0x1D41A, 0x1D7CE)
        case .italic: (0x1D434, 0x1D44E, nil)
        case .boldItalic: (0x1D468, 0x1D482, nil)
        case .script: (0x1D49C, 0x1D4B6, nil)
        case .monospace: (0x1D670, 0x1D68A, 0x1D7F6)
        case .doubleStruck: (0x1D538, 0x1D552, 0x1D7D8)
        case .circled: (0x24B6, 0x24D0, nil)
        case .strikethrough, .underline: nil
        }
    }

    /// Letters Unicode encoded before the mathematical alphabets, which leave a hole there.
    private var exceptions: [Character: UInt32] {
        switch self {
        case .italic: ["h": 0x210E]
        case .script: [
            "B": 0x212C, "E": 0x2130, "F": 0x2131, "H": 0x210B, "I": 0x2110, "L": 0x2112, "M": 0x2133,
            "R": 0x211B, "e": 0x212F, "g": 0x210A, "o": 0x2134,
        ]
        case .doubleStruck: ["C": 0x2102, "H": 0x210D, "N": 0x2115, "P": 0x2119, "Q": 0x211A, "R": 0x211D, "Z": 0x2124]
        default: [:]
        }
    }

    /// The text in this style alone: a style the text already has gives way to it.
    func apply(to text: String) -> String {
        let text = Self.plain(text)
        switch self {
        case .strikethrough: return combining(text, mark: "\u{0336}")
        case .underline: return combining(text, mark: "\u{0332}")
        default: break
        }
        guard let bases else { return text }
        let exceptions = exceptions
        var output = String.UnicodeScalarView()
        for scalar in text.unicodeScalars {
            let value = scalar.value
            let mapped: UInt32? = if let exception = exceptions[Character(scalar)] {
                exception
            } else if (0x41...0x5A).contains(value) {
                bases.upper + value - 0x41
            } else if (0x61...0x7A).contains(value) {
                bases.lower + value - 0x61
            } else if let digit = bases.digit, (0x30...0x39).contains(value) {
                digit + value - 0x30
            } else { nil }
            output.append(mapped.flatMap(Unicode.Scalar.init) ?? scalar)
        }
        return String(output)
    }

    /// Text with every style's look-alikes and marks turned back into plain letters and digits.
    static func plain(_ text: String) -> String {
        var output = String.UnicodeScalarView()
        for scalar in text.unicodeScalars where scalar != "\u{0336}" && scalar != "\u{0332}" {
            if styled.contains(scalar.value) {
                output.append(contentsOf: String(scalar).precomposedStringWithCompatibilityMapping.unicodeScalars)
            } else {
                output.append(scalar)
            }
        }
        return String(output)
    }

    /// The mathematical letters and digits, the circled letters, and the letterlike symbols the
    /// styles borrow, each of which Unicode's compatibility mapping turns back into its letter.
    private static let styled = Set<UInt32>(0x1D400...0x1D7FF).union(0x24B6...0x24E9)
        .union(allCases.flatMap(\.exceptions.values))

    private func combining(_ text: String, mark: Character) -> String {
        var output = ""
        for character in text {
            output.append(character)
            if !character.isWhitespace && !character.isNewline { output.append(mark) }
        }
        return output
    }
}

extension TypingText {
    /// The keyword a snippet would expand: everything since the last space or line break.
    static func keyword(before text: String) -> Substring? {
        let start = text.lastIndex(where: \.isWhitespace).map(text.index(after:)) ?? text.startIndex
        let keyword = text[start...]
        return keyword.isEmpty ? nil : keyword
    }
}

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

    /// Dictation listens in English and Russian only.
    static func offered(_ language: String) -> Bool {
        ["en", "ru"].contains(language.prefix { $0 != "-" && $0 != "_" })
    }

    static var languages: [String] {
        get {
            let stored = (UserDefaults.standard.stringArray(forKey: languagesKey) ?? [Locale.current.identifier]).filter(offered)
            return stored.isEmpty ? ["en-US"] : stored
        }
        set {
            guard !newValue.isEmpty else { return }
            UserDefaults.standard.set(newValue, forKey: languagesKey)
            if !newValue.contains(activeLanguage) { activeLanguage = newValue[0] }
        }
    }
    static var activeLanguage: String {
        get { UserDefaults.standard.string(forKey: activeLanguageKey).flatMap { offered($0) ? $0 : nil } ?? languages[0] }
        set { UserDefaults.standard.set(newValue, forKey: activeLanguageKey) }
    }
    static var warmMinutes: Int {
        let stored = UserDefaults.standard.integer(forKey: minutesKey)
        return [1, 5, 15, 30].contains(stored) ? stored : 5
    }

    /// "Dictate from the keyboard", which the keyboard reads too now that it listens itself.
    static var shared: UserDefaults { UserDefaults(suiteName: KeyboardDraftStore.group) ?? .standard }
    static var keyboardEnabled: Bool { shared.bool(forKey: keyboardKey) }

    static let microphoneAllowedKey = "sona.keyboard.microphoneAllowed"
    /// Whether Sona holds the microphone and speech recognition permissions the keyboard
    /// listens under. The app says so: a keyboard process that asked before they were
    /// granted keeps its first answer until iOS ends it.
    static var keyboardMicrophoneAllowed: Bool {
        get { shared.bool(forKey: microphoneAllowedKey) }
        set { shared.set(newValue, forKey: microphoneAllowedKey) }
    }

    /// The switch lived in the app's own defaults until the keyboard needed to read it.
    static func moveKeyboardSwitchToSharedContainer() {
        guard let enabled = UserDefaults.standard.object(forKey: keyboardKey) as? Bool else { return }
        if shared.object(forKey: keyboardKey) == nil { shared.set(enabled, forKey: keyboardKey) }
        UserDefaults.standard.removeObject(forKey: keyboardKey)
    }

    /// The style every dictation uses unless one recording asks for another.
    static var style: String { shared.string(forKey: styleKey) ?? "" }

    /// The style lived in the app's own defaults until the keyboard needed to read it.
    static func moveStyleToSharedContainer() {
        guard let style = UserDefaults.standard.string(forKey: styleKey) else { return }
        if shared.object(forKey: styleKey) == nil { shared.set(style, forKey: styleKey) }
        UserDefaults.standard.removeObject(forKey: styleKey)
    }

    /// The recognizer language for a dictation. One the keyboard asked for while typing
    /// `typing` listens in that language, in the regional form the speaking list prefers.
    static func recognitionLanguage(for typing: String?) -> String {
        guard let typing else { return activeLanguage }
        let base = typing.prefix { $0 != "-" && $0 != "_" }
        return ([activeLanguage] + languages).first { $0.prefix { $0 != "-" && $0 != "_" } == base } ?? typing
    }
}


/// The phone's cleanup of what Apple's recognizer heard, before any style. It drops
/// the hesitation sounds the Mac drops, says once a small word restarted after a
/// comma ("the, the"), and sets spoken ordinals ("first …, second …") as a
/// numbered list, which the Mac leaves to its cleanup prompts. Measured on a
/// 26-second ramble: the phone pasted "Ah", "um" and "Um" as said, and the
/// on-device style rewrite left "1st the pricing page, 2nd the FAQ" inline in six
/// of six runs.
enum DictationCleanup {
    /// Not a word in any language Sona's recognizers write. Mirrors
    /// `UNIVERSAL_FILLER_WORDS` in src-tauri/src/audio_toolkit/text.rs.
    private static let universalFillers = [
        "uh", "uhm", "umm", "uhh", "uhhh", "ehh", "ehm", "ahm", "hmm", "hm", "mmm", "хм", "ммм",
    ]
    private static let ordinals = [
        ["first", "1st"], ["second", "2nd"], ["third", "3rd"], ["fourth", "4th"], ["fifth", "5th"],
        ["sixth", "6th"], ["seventh", "7th"], ["eighth", "8th"], ["ninth", "9th"], ["tenth", "10th"],
    ]
    /// Small words a speaker restarts on. "Very, very" and "no, no" are said twice
    /// on purpose and stay.
    private static let restart = try? NSRegularExpression(
        pattern: #"(?i)\b(the|a|an|i|we|you|it|to|and|but|so|that|this|in|on|of|for|with|my|our|is|was), \1\b"#)
    private static let spaces = try? NSRegularExpression(pattern: #"[^\S\n]{2,}"#)
    private static let ordinalItem = try? NSRegularExpression(
        pattern: #"(?i)(^|[.,;:]\s+)(?:and\s+)?("# + ordinals.joined().joined(separator: "|") + #")\b(?!\s+of\b),?\s+"#)

    /// The hesitation sounds safe to drop for `language`, a BCP 47 tag. Mirrors
    /// `gated_filler_words_for_language`: "um" is a word in Portuguese and German.
    private static func fillers(for language: String) -> [String] {
        switch language.split(whereSeparator: { $0 == "-" || $0 == "_" }).first {
        case "en": universalFillers + ["um", "ah", "eh"]
        case "de": universalFillers + ["äh", "ähm"]
        case "fr": universalFillers + ["euh"]
        default: universalFillers
        }
    }

    static func clean(_ text: String, language: String) -> String {
        numbered(restartsJoined(fillersRemoved(text, language: language)))
    }

    /// The text of a style rewrite. Sona's cleanup contract asks for
    /// `{"transcription": …}` when a structured answer is wanted, and the
    /// on-device model sent that object back for a plain request in three of
    /// three runs; the phone pasted the braces.
    static func rewritten(_ reply: String) -> String {
        let trimmed = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("{"),
              let object = try? JSONDecoder().decode([String: String].self, from: Data(trimmed.utf8)),
              let text = object["transcription"]?.trimmingCharacters(in: .whitespacesAndNewlines),
              !text.isEmpty
        else { return trimmed }
        return text
    }

    /// A style rewrite of `source` without what the on-device model made up
    /// despite the prompt: a "Subject:" line and lines holding a [placeholder],
    /// unless the source had them. Asked not to, it still wrote them in 3 of 10
    /// Email rewrites.
    static func withoutInventions(_ text: String, source: String) -> String {
        let keepsSubject = source.range(of: "subject", options: .caseInsensitive) != nil
        let keepsBrackets = source.contains("[")
        let lines = text.components(separatedBy: "\n").filter { line in
            (keepsBrackets || line.range(of: #"\[[^\]\n]+\]"#, options: .regularExpression) == nil)
                && (keepsSubject || line.range(of: #"^\s*subject\s*:"#, options: [.regularExpression, .caseInsensitive]) == nil)
        }
        return lines.joined(separator: "\n")
            .replacingOccurrences(of: #"\n{3,}"#, with: "\n\n", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Each filler and the comma or point after it gone. A filler that opened a
    /// sentence hands its capital to the word after it: "Um, can you" is "Can you".
    private static func fillersRemoved(_ text: String, language: String) -> String {
        var output = text
        for filler in fillers(for: language) {
            guard let pattern = try? NSRegularExpression(
                pattern: #"(?i)\b"# + NSRegularExpression.escapedPattern(for: filler) + #"\b[,.]?"#)
            else { continue }
            var kept = ""
            var resume = output.startIndex
            var capitalOwed = false
            for match in pattern.matches(in: output, range: NSRange(output.startIndex..., in: output)) {
                guard let range = Range(match.range, in: output) else { continue }
                append(output[resume..<range.lowerBound], to: &kept, capitalOwed: &capitalOwed)
                capitalOwed = capitalOwed || opensSentence(kept)
                resume = range.upperBound
            }
            append(output[resume...], to: &kept, capitalOwed: &capitalOwed)
            output = kept
        }
        let range = NSRange(output.startIndex..., in: output)
        output = spaces?.stringByReplacingMatches(in: output, range: range, withTemplate: " ") ?? output
        return output.replacingOccurrences(of: " ,", with: ",").trimmingCharacters(in: .whitespaces)
    }

    private static func opensSentence(_ text: String) -> Bool {
        guard let last = text.last(where: { !$0.isWhitespace }) else { return true }
        return ".!?…".contains(last)
    }

    /// Only an ASCII letter takes the capital, as on the Mac: "Um, iPhone" is
    /// "iPhone", and the debt is settled by the first letter or digit either way.
    private static func append(_ text: Substring, to output: inout String, capitalOwed: inout Bool) {
        guard capitalOwed, let first = text.firstIndex(where: { $0.isLetter || $0.isNumber }) else {
            output += text
            return
        }
        capitalOwed = false
        let capital = text[first].isASCII && text[first].isLowercase ? text[first].uppercased() : String(text[first])
        output += text[..<first] + capital + text[text.index(after: first)...]
    }

    /// "the, the screenshots" is "the screenshots": a word said again after a comma
    /// is a restart, not two words.
    private static func restartsJoined(_ text: String) -> String {
        restart?.stringByReplacingMatches(in: text, range: NSRange(text.startIndex..., in: text), withTemplate: "$1") ?? text
    }

    /// Two or more clauses that open with "first", "second" and so on, in order, as
    /// a numbered list on lines of their own. Ordinals out of order, or a lone
    /// "first", stay as said.
    private static func numbered(_ text: String) -> String {
        guard let ordinalItem else { return text }
        let matches = ordinalItem.matches(in: text, range: NSRange(text.startIndex..., in: text))
        var run: [(separator: Range<String.Index>, body: String.Index)] = []
        for match in matches {
            guard let whole = Range(match.range, in: text), let word = Range(match.range(at: 2), in: text),
                  let lead = Range(match.range(at: 1), in: text),
                  let rank = ordinals.firstIndex(where: { $0.contains(text[word].lowercased()) })
            else { continue }
            if rank == run.count {
                run.append((lead.lowerBound..<whole.upperBound, whole.upperBound))
            } else if run.count < 2 {
                run = rank == 0 ? [(lead.lowerBound..<whole.upperBound, whole.upperBound)] : []
            } else {
                break
            }
        }
        guard run.count >= 2, let last = run.last else { return text }
        // The list ends where its sentence does: a stop, "!", "?" or ";" before a
        // space or the end, never the point inside "9.99" or "sona.app".
        let end = text[last.body...].indices.first { at in
            ".!?;".contains(text[at]) && text.index(after: at) == text.endIndex
                || ".!?;".contains(text[at]) && text[text.index(after: at)].isWhitespace
        } ?? text.endIndex
        let asks = end < text.endIndex && "?!".contains(text[end])
        var items: [String] = []
        for (index, item) in run.enumerated() {
            let stop = index + 1 < run.count ? run[index + 1].separator.lowerBound : end
            var words = text[item.body..<stop].trimmingCharacters(in: CharacterSet(charactersIn: " ,;.\n"))
            if asks, index + 1 == run.count { words.append(text[end]) }
            guard let first = words.first else { return text }
            items.append("\(index + 1). \(first.uppercased())\(words.dropFirst())")
        }
        let before = text[..<run[0].separator.lowerBound].trimmingCharacters(in: .whitespaces)
        var after = String(text[end...].drop(while: { ".!?;".contains($0) })).trimmingCharacters(in: .whitespaces)
        if let first = after.first { after = first.uppercased() + after.dropFirst() }
        let lead = before.isEmpty ? "" : (before.last.map { ".!?:".contains($0) } == true ? String(before.dropLast()) : before) + ":\n"
        return lead + items.joined(separator: "\n") + (after.isEmpty ? "" : "\n\n" + after)
    }
}

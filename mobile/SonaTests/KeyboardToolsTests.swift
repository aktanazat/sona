import Foundation
import Testing

@MainActor
struct KeyboardToolsTests {
    /// The text in a field after `edit` runs on `text`.
    static func applying(_ edit: TypingSession.Edit, to text: String) -> String {
        String(text.dropLast(edit.deleteCount)) + edit.insert
    }

    @Test func snippetKeywordExpandsOnSpaceAndBackspaceKeepsTheKeyword() {
        var session = TypingSession(corrector: nil)
        session.snippets = ["@@addr": "12 Grove Street"]
        var text = "Send it to @@addr"
        text = Self.applying(session.end(with: " ", before: text, corrects: true), to: text)
        #expect(text == "Send it to 12 Grove Street ")
        text = Self.applying(session.backspace(before: text), to: text)
        #expect(text == "Send it to @@addr")
        text = Self.applying(session.end(with: " ", before: text, corrects: true), to: text)
        #expect(text == "Send it to @@addr ")
    }

    /// The phone's tone texts must stay the Mac's, which renders the same blocks.
    @Test(arguments: [DictationTone.casual, .semiCasual, .semiFormal, .formal])
    func toneRuleMatchesTheMacPrompt(_ tone: DictationTone) throws {
        let prompts = URL(filePath: #filePath).deletingLastPathComponent()
            .appending(path: "../../src-tauri/resources/prompts").standardized
        let rule = try String(contentsOf: prompts.appending(path: "tone_\(tone.rawValue).txt"), encoding: .utf8)
        let header = try String(contentsOf: prompts.appending(path: "tone_header.txt"), encoding: .utf8)
        #expect(tone.rule == rule)
        #expect(DictationTone.header == header)
    }

    @Test func phoneToneReplacesTheToneTheMacWrote() {
        let mac = "Rewrite as a message.\n\n" + DictationTone.header + DictationTone.casual.rule!
            + "\n\nPunctuation policy: normal."
        let phone = DictationTone.formal.applied(to: mac)
        #expect(phone == "Rewrite as a message.\n\nPunctuation policy: normal.\n\n" + DictationTone.header + DictationTone.formal.rule!)
        #expect(DictationTone.written(in: mac) == .casual)
        #expect(DictationTone.balanced.applied(to: mac) == "Rewrite as a message.\n\nPunctuation policy: normal.")
    }

    /// Letters Unicode placed outside the mathematical alphabets, and plain text around them.
    @Test(arguments: [
        (TextStyle.script, "Hello, Rob 9", "ℋℯ𝓁𝓁ℴ, ℛℴ𝒷 9"),
        (.italic, "high", "ℎ𝑖𝑔ℎ"),
        (.doubleStruck, "CPR 42", "ℂℙℝ 𝟜𝟚"),
        (.bold, "Ok 1!", "𝐎𝐤 𝟏!"),
        (.monospace, "é a", "é 𝚊"),
    ])
    func textStyleMapsLettersToTheirLookAlikes(_ style: TextStyle, _ input: String, _ expected: String) {
        #expect(style.apply(to: input) == expected)
    }

    @Test(arguments: [
        (TextCase.sentence, "HELLO THERE. SO i'M here, i THINK!\nnew LINE", "Hello there. So I'm here, I think!\nNew line"),
        (.title, "the QUICK fox's tail", "The Quick Fox's Tail"),
    ])
    func caseChangeKeepsSentencesAndWords(_ change: TextCase, _ input: String, _ expected: String) {
        #expect(change.apply(to: input, locale: Locale(identifier: "en_US")) == expected)
    }
}

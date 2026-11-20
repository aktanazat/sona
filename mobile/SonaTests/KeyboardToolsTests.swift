import Foundation
import Testing
import UIKit

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

    @Test func capitalizedKeywordCapitalizesItsExpansion() {
        var session = TypingSession(corrector: nil)
        session.snippets = ["ofc": "office"]
        let text = "See you there. Ofc"
        #expect(Self.applying(session.end(with: " ", before: text, corrects: true), to: text) == "See you there. Office ")
    }

    /// The on-device model adds a subject and placeholders to Email rewrites even when told not to.
    @Test func styleRewriteDropsInventedSubjectAndPlaceholders() {
        let reply = "Subject: Lunch Schedule Change\n\nHi [Recipient's Name],\n\nCan we move lunch to Friday?\n\nBest regards,\n[Your Name]"
        #expect(DictationCleanup.withoutInventions(reply, source: "Can we move lunch to Friday?")
            == "Can we move lunch to Friday?\n\nBest regards,")
        #expect(DictationCleanup.withoutInventions("Subject: Q3 budget\n\nIt is approved.", source: "subject Q3 budget it is approved")
            == "Subject: Q3 budget\n\nIt is approved.")
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

    /// A style replaces the one the text had, so Italic on bold text gives italic text.
    @Test(arguments: [
        (TextStyle.italic, "𝐎𝐤 𝟏", "𝑂𝑘 1"),
        (.bold, "ℋℯ𝓁𝓁ℴ", "𝐇𝐞𝐥𝐥𝐨"),
        (.monospace, "a̶b̶", "𝚊𝚋"),
    ])
    func textStyleReplacesTheStyleTheTextHad(_ style: TextStyle, _ input: String, _ expected: String) {
        #expect(style.apply(to: input) == expected)
    }

    /// Styled letters take two UTF-16 units each, and the host moves the cursor by those units.
    @Test func replacingWithTheCursorBeforeStyledTextReplacesTheWholeField() throws {
        let field = Field(before: "I have", after: " 𝐠𝐨𝐧𝐞 home")
        let target = field.view().target
        field.perform(try #require(field.view().replacing(target, with: "We went home")))
        #expect(field.text == "We went home")
        field.perform(try #require(field.view().undoing(FieldText.Change(original: target.text, replacement: "We went home"))))
        #expect(field.text == "I have 𝐠𝐨𝐧𝐞 home")
    }

    /// A host that has not caught up still shows the field from before the keyboard's last
    /// edit; a tool working from that report would cut the wrong text or glue its words on.
    @Test func toolChangeWhileTheHostIsBehindNeitherGluesNorCorrupts() throws {
        let field = Field(before: "Hi", after: "")
        var echo = FieldEcho()
        echo.edited(before: "Hi", after: "", deleting: 0, inserting: " there ")
        field.insertText(" there ")
        let behind = FieldText(hostBefore: "Hi", hostAfter: "", selection: nil, echo: echo)
        #expect(behind.inserting("friend").edit.insert == "friend")
        field.perform(try #require(behind.replacing(behind.target, with: "Hello there ")))
        #expect(field.text == "Hello there ")
    }

    @Test(arguments: [
        (TextCase.sentence, "HELLO THERE. SO i'M here, i THINK!\nnew LINE", "Hello there. So I'm here, I think!\nNew line"),
        (.title, "the QUICK fox's tail", "The Quick Fox's Tail"),
        (.title, "can we move lunch to the office on friday", "Can We Move Lunch to the Office on Friday"),
    ])
    func caseChangeKeepsSentencesAndWords(_ change: TextCase, _ input: String, _ expected: String) {
        #expect(change.apply(to: input, locale: Locale(identifier: "en_US")) == expected)
    }
}

/// A text field as a host shows it to the keyboard: the cursor moves by UTF-16 units and
/// a backspace deletes one character.
private final class Field: NSObject, UITextDocumentProxy {
    private var before: String
    private var after: String

    init(before: String, after: String) {
        self.before = before
        self.after = after
    }

    var text: String { before + after }
    var documentContextBeforeInput: String? { before }
    var documentContextAfterInput: String? { after }
    var selectedText: String? { nil }
    var documentInputMode: UITextInputMode? { nil }
    var documentIdentifier = UUID()
    var hasText: Bool { !text.isEmpty }

    func adjustTextPosition(byCharacterOffset offset: Int) {
        var moved = 0
        while moved < offset, let scalar = after.unicodeScalars.first {
            before.unicodeScalars.append(scalar)
            after.unicodeScalars.removeFirst()
            moved += scalar.utf16.count
        }
    }

    func insertText(_ text: String) { before += text }
    func deleteBackward() { if !before.isEmpty { before.removeLast() } }
    func setMarkedText(_ markedText: String, selectedRange: NSRange) {}
    func unmarkText() {}

    /// The field as the tools see it from a host that is up to date.
    func view() -> FieldText {
        FieldText(hostBefore: before, hostAfter: after, selection: nil, echo: FieldEcho())
    }

    /// A tools step, carried out as the keyboard carries it out.
    func perform(_ step: FieldText.Step) {
        adjustTextPosition(byCharacterOffset: step.move)
        for _ in 0..<step.edit.deleteCount { deleteBackward() }
        insertText(step.edit.insert)
    }
}

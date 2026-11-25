import Foundation
import Testing
import UIKit

@MainActor
struct KeyboardLearningTests {
    private func session(_ learning: KeyboardLearning = KeyboardLearning()) throws -> TypingSession {
        TypingSession(corrector: try #require(TypingCorrector(language: "en-US")), learning: learning)
    }

    @Test("a stretched word stays as typed", arguments: ["sooo", "yesss", "nooooo", "omggg"])
    func stretched(word: String) throws {
        var session = try session()
        var text = "that is " + word
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "that is \(word) ")
    }

    /// The corrector as it is once the language's common words have loaded.
    private func warmSession(_ learning: KeyboardLearning = KeyboardLearning()) async throws -> TypingSession {
        let corrector = try #require(TypingCorrector(language: "en-US"))
        corrector.common = await CommonWords.list(for: .english)
        return TypingSession(corrector: corrector, learning: learning)
    }

    @Test("a word typed twice is learned and offered as the completion of its start")
    func learnsTypedWords() async throws {
        var session = try await warmSession()
        for _ in 0..<2 {
            var text = "ping aktanbot"
            text.apply(session.end(with: " ", before: text, corrects: true))
        }
        #expect(session.learning.knows("aktanbot"))
        #expect(session.suggestions(before: "ask akta")?.others.contains("aktanbot") == true)
    }

    @Test("a typo spaced past before the common words load is not learned, so it is corrected later")
    func typoBeforeTheWordsLoadIsNotLearned() async throws {
        var cold = try session()
        for _ in 0..<2 {
            var text = "so teh"
            text.apply(cold.end(with: " ", before: text, corrects: true))
        }
        var session = try await warmSession(cold.learning)
        var text = "so teh"
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "so the ")
    }

    @Test("typos learned by mistake are forgotten once, and learned names stay known")
    func learnedTyposAreForgotten() async throws {
        var learning = KeyboardLearning()
        for word in ["teh", "teh", "Dauren", "Dauren"] { learning.typed(word, after: nil) }
        var session = try await warmSession(learning)
        session.forgetLearnedTypos()
        #expect(!session.learning.knows("teh"))
        #expect(session.learning.knows("Dauren"))
        var text = "so teh"
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "so the ")
    }

    @Test("a lone j is the word I", arguments: ["j", "J"])
    func loneJIsI(typed: String) throws {
        var session = try session()
        var text = "yes " + typed
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "yes I ")
    }

    @Test("coming back to a corrected word offers the word as typed, and choosing it keeps it")
    func undoCorrectionLater() throws {
        var session = try session()
        var text = "see you tomorow"
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "see you tomorrow ")
        text += "then"
        let revisited = "see you tomorrow"
        let offer = try #require(session.suggestions(before: revisited))
        #expect(offer.restore == "tomorow")
        var field = revisited
        field.apply(session.choose("tomorow", before: field))
        #expect(field == "see you tomorow ")
        #expect(session.learning.knows("tomorow"))
    }

    @Test("undoing the second correction leaves the first occurrence available to restore")
    func undoLeavesEarlierOccurrence() throws {
        var session = try session()
        var text = "see you tomorow"
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "see you tomorrow ")
        text += "then tomorow"
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "see you tomorrow then tomorrow ")
        text.apply(session.backspace(before: text))
        #expect(text == "see you tomorrow then tomorow")
        #expect(session.suggestions(before: "see you tomorrow")?.restore == "tomorow")
    }

    @Test("after a space the strip offers the words that usually come next, his own first")
    func nextWords() throws {
        let table = NextWords(contents: "want to\tgo be do\nwant\tto a\n\tI the\n")
        var learning = KeyboardLearning()
        learning.typed("call", after: "want")
        learning.typed("call", after: "want")
        let session = TypingSession(corrector: nil, learning: learning, predictor: table)
        #expect(session.suggestions(before: "I want ")?.others == ["call", "to", "a"])
        #expect(session.suggestions(before: "we want to ")?.others == ["go", "be", "do"])
        #expect(session.suggestions(before: "Done. ")?.others == ["I", "the"])
    }

    @Test("the bundled tables predict the common next word in English and Russian")
    func bundledNextWords() async throws {
        let english = try #require(await NextWords.table(for: .english))
        #expect(english.next(after: ["thank"], limit: 3).first == "you")
        let russian = try #require(await NextWords.table(for: .russian))
        #expect(!russian.next(after: ["я"], limit: 3).isEmpty)
    }

    @Test("a slide on delete takes the word before the cursor and its trailing spaces")
    func wordDeletion() {
        #expect(TypingSession.wordDeletion(before: "hello there  ") == 7)
        #expect(TypingSession.wordDeletion(before: "hello there") == 5)
        #expect(TypingSession.wordDeletion(before: "hi!") == 1)
    }

    @Test("a slide on delete over a selection takes the selection first, and sliding back restores the text exactly")
    func slideOverSelection() throws {
        let field = FieldDouble(before: "see you at ", selected: "noon")
        var echo = FieldEcho()
        let first = try #require(field.takeWord(echo: &echo))
        echo.hostNoticed()
        let second = try #require(field.takeWord(echo: &echo))
        #expect(first == "noon")
        #expect(field.text == "see you ")
        field.apply(TypingSession.Edit(insert: second), echo: &echo)
        field.apply(TypingSession.Edit(insert: first), echo: &echo)
        #expect(field.text == "see you at noon")
    }

    @Test("after a selection, a slide on delete takes the next word only once the host has reported the change")
    func slideWaitsForHostAfterSelection() throws {
        let field = FieldDouble(before: "see you at ", selected: "noon")
        var echo = FieldEcho()
        #expect(field.takeWord(echo: &echo) == "noon")
        #expect(field.takeWord(echo: &echo) == nil)
        #expect(field.text == "see you at ")
        echo.hostNoticed()
        #expect(field.takeWord(echo: &echo) == "at ")
    }

    @Test("a slide on delete over a long selection takes it in one delete, with nothing to put back")
    func slideOverLongSelection() throws {
        let note = String(repeating: "a long note ", count: 100)
        let field = FieldDouble(before: "keep ", selected: note)
        var echo = FieldEcho()
        #expect(field.takeWord(echo: &echo) == "")
        #expect(field.edits == 1)
        #expect(field.text == "keep")
    }

    @Test("the typed form of a correction is offered only at that word, so a later same word cannot teach the typo")
    func restoreOnlyAtCorrectedWord() throws {
        var session = try session()
        var text = "see you tomorow"
        text.apply(session.end(with: " ", before: text, corrects: true))
        text += "and tomorrow"
        #expect(session.suggestions(before: text)?.restore == nil)
        text.apply(session.choose("tomorow", before: text))
        #expect(!session.learning.knows("tomorow"))
    }

    @Test("a cursor move keeps the correction on offer; text the keyboard did not write forgets it")
    func restoreForgottenOnHostChange() throws {
        var session = try session()
        let field = UUID()
        session.hostChanged(field: field, text: "")
        var text = "tomorow"
        text.apply(session.end(with: " ", before: text, corrects: true))
        session.edited(field: field, text: text)
        session.hostChanged(field: field, text: text)
        #expect(session.suggestions(before: "tomorrow")?.restore == "tomorow")
        session.hostChanged(field: field, text: "")
        #expect(session.suggestions(before: "tomorrow")?.restore == nil)
    }

    @Test("a correction is not offered back in another field with the same text")
    func restoreTiedToField() throws {
        var session = try session()
        session.hostChanged(field: UUID(), text: "")
        var text = "tomorow"
        text.apply(session.end(with: " ", before: text, corrects: true))
        session.edited(field: UUID(), text: text)
        #expect(session.suggestions(before: "tomorrow")?.restore == nil)
    }

    @Test("a Reset in Sona wins over the keyboard saving the learning it read before the reset")
    func resetWinsOverStaleSave() throws {
        let suite = "sona-learning-\(UUID())"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = KeyboardLearningStore(defaults: defaults)
        var learning = store.load().learning
        learning.keep("aktanbot")
        #expect(try store.save(learning, readAt: store.load().resets))
        let opened = store.load()
        store.reset()
        #expect(try store.save(opened.learning, readAt: opened.resets) == false)
        #expect(!store.load().learning.knows("aktanbot"))
    }
}

@MainActor
struct RussianTypingTests {
    private func session() async throws -> TypingSession {
        let corrector = try #require(TypingCorrector(language: TypingLanguage.russian.rawValue))
        corrector.common = await CommonWords.list(for: .russian)
        return TypingSession(corrector: corrector)
    }

    @Test("a Russian typo is corrected as the word ends", arguments: [
        ("превет", "привет"), ("спосибо", "спасибо"), ("сегодя", "сегодня"),
    ])
    func corrects(typed: String, intended: String) async throws {
        var session = try await session()
        var text = "ну " + typed
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "ну \(intended) ")
    }

    @Test("a Russian word being typed offers its completions")
    func completes() async throws {
        let session = try await session()
        #expect(session.suggestions(before: "ну спас")?.others.contains("спасибо") == true)
    }

    @Test("the Russian next words leave out Tatoeba's names")
    func nextWordsWithoutNames() async throws {
        let table = try #require(await NextWords.table(for: .russian))
        #expect(table.next(after: [], limit: 3) == ["я", "ты", "он"])
        #expect(table.next(after: ["как"], limit: 3) == ["ты", "вы", "я"])
    }

    @Test("Russian chat words stay as typed", arguments: ["щас", "чё", "прога", "пж", "ок", "спс", "кст", "оч"])
    func chatWordsStay(word: String) async throws {
        var session = try await session()
        var text = "ну " + word
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "ну \(word) ")
    }
}

@MainActor
struct EnglishWordListTests {
    private func corrector() async throws -> TypingCorrector {
        let corrector = try #require(TypingCorrector(language: TypingLanguage.english.rawValue))
        corrector.common = await CommonWords.list(for: .english)
        return corrector
    }

    @Test("a misspelling is corrected to the word, never to a misspelling in the word list", arguments: [
        "occuted", "occurrd", "occired", "ocured",
    ])
    func correctsToTheWord(typed: String) async throws {
        let corrector = try await corrector()
        #expect(corrector.correction(before: "it " + typed, lexicon: TypingLexicon())?.replacement == "occurred")
    }

    @Test("a letter tripled inside a word is a typo, and a guess that only capitalizes it does not block the fix", arguments: [
        ("after schoool", "school"), ("order pizzza", "pizza"), ("so goood", "good"), ("a special occassion", "occasion"),
    ])
    func fixesTypoNotEmphasis(text: String, intended: String) async throws {
        let corrector = try await corrector()
        #expect(corrector.correction(before: text, lexicon: TypingLexicon())?.replacement == intended)
    }

    @Test("a proper noun typed in lowercase is capitalized, not changed into another word", arguments: [
        ("see you monday", "Monday"), ("in december", "December"), ("from texas", "Texas"),
    ])
    func capitalizesProperNouns(text: String, intended: String) async throws {
        let corrector = try await corrector()
        #expect(corrector.correction(before: text, lexicon: TypingLexicon())?.replacement == intended)
    }

    @Test("completions and swipe words from the English list are all words the spell checker accepts")
    func listHoldsNoMisspellings() async throws {
        let corrector = try await corrector()
        let checker = UITextChecker()
        let letters = "abcdefghijklmnopqrstuvwxyz".map(String.init)
        let offered = Set((letters + letters.flatMap { first in letters.map { first + $0 } }).flatMap(corrector.completions(of:)))
        let rejected = offered.filter { word in
            checker.rangeOfMisspelledWord(
                in: word, range: NSRange(location: 0, length: word.utf16.count),
                startingAt: 0, wrap: false, language: corrector.language
            ).location != NSNotFound
        }
        #expect(rejected.isEmpty, "\(rejected.sorted())")
    }
}

struct KeyboardLanguagesTests {
    @Test("stored languages the keyboard no longer has count as English", arguments: [
        (["ja-JP"], [TypingLanguage.english]),
        (["fr-FR", "ru-RU"], [.english, .russian]),
        (["ru-RU"], [.russian]),
    ])
    func unknownFallsBackToEnglish(stored: [String], enabled: [TypingLanguage]) throws {
        let suite = "sona-languages-\(UUID())"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.set(stored, forKey: KeyboardLanguages.enabledKey)
        #expect(KeyboardLanguages(defaults: defaults).enabled == enabled)
    }
}

/// A text field as a keyboard sees it: text before the cursor and a selection after it.
/// Delete on a selection removes it with the space before it, as Messages does; typing
/// replaces it.
private final class FieldDouble: NSObject, UITextDocumentProxy {
    private var head: String
    private var selection: String

    init(before: String, selected: String) {
        head = before
        selection = selected
    }

    var text: String { head + selection }
    /// Calls that changed the field.
    private(set) var edits = 0
    var documentContextBeforeInput: String? { head }
    var documentContextAfterInput: String? { nil }
    var selectedText: String? { selection.isEmpty ? nil : selection }
    var documentInputMode: UITextInputMode? { nil }
    var documentIdentifier: UUID { UUID() }
    var hasText: Bool { !text.isEmpty }

    func insertText(_ text: String) {
        edits += 1
        selection = ""
        head += text
    }

    func deleteBackward() {
        edits += 1
        if !selection.isEmpty {
            selection = ""
            if head.hasSuffix(" ") { head.removeLast() }
        } else if !head.isEmpty {
            head.removeLast()
        }
    }

    func adjustTextPosition(byCharacterOffset offset: Int) {}
    func setMarkedText(_ markedText: String, selectedRange: NSRange) {}
    func unmarkText() {}
}

@MainActor
struct EmojiCatalogTests {
    @Test("the strip offers the emoji named by a typed word")
    func suggest() async throws {
        let catalog = try #require(await EmojiCatalog.shared.value)
        #expect(catalog.emoji(for: "Pizza") == "\u{1F355}")
    }
}

private extension String {
    mutating func apply(_ edit: TypingSession.Edit) {
        removeLast(edit.deleteCount)
        append(edit.insert)
    }
}

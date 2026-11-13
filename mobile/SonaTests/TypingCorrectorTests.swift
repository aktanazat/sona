import Foundation
import Testing

@MainActor
struct TypingCorrectorTests {
    let corrector: TypingCorrector

    init() throws {
        corrector = try #require(TypingCorrector(language: "en-US"))
    }

    @Test("space fixes at least 9 in 10 typos written after tuning and turns at most 1 in 20 into a wrong word")
    func freshTypos() {
        let outcome = outcome(of: TypingSamples.freshTypos)
        #expect(outcome.fixed * 10 >= TypingSamples.freshTypos.count * 9, "\(outcome)")
        #expect(outcome.wrong.count * 20 <= TypingSamples.freshTypos.count, "\(outcome)")
    }

    @Test("space keeps fixing the typos the weights were tuned on")
    func tunedTypos() {
        let outcome = outcome(of: TypingSamples.typos)
        #expect(outcome.fixed * 100 >= TypingSamples.typos.count * 92, "\(outcome)")
        #expect(outcome.wrong.count * 20 <= TypingSamples.typos.count, "\(outcome)")
    }

    @Test(
        "correct words, names, vocabulary, numbers, addresses and all-caps words stay as typed",
        arguments: TypingSamples.keeps + TypingSamples.freshKeeps
    )
    func keeps(text: String) {
        #expect(corrector.correction(before: text, lexicon: TypingSamples.lexicon) == nil)
    }

    @Test("a vocabulary word the dictionary would change stays as typed once the Mac sends it")
    func vocabularyWins() {
        #expect(corrector.correction(before: "run omp", lexicon: TypingLexicon()) != nil)
        #expect(corrector.correction(before: "run omp", lexicon: TypingLexicon(vocabulary: ["omp"])) == nil)
    }

    private func outcome(of typos: [TypingSamples.Typo]) -> (fixed: Int, wrong: [String]) {
        var fixed = 0
        var wrong: [String] = []
        for typo in typos {
            switch corrector.correction(before: typo.text, lexicon: TypingSamples.lexicon)?.replacement {
            case typo.intended: fixed += 1
            case nil: break
            case let other?: wrong.append("\(typo.text) -> \(other)")
            }
        }
        return (fixed, wrong)
    }
}

@MainActor
struct TypingSessionTests {
    @Test(
        "backspace right after a correction puts the typed word back, and that word is not corrected again",
        arguments: [("see you tomorow", "see you tomorrow ", "tomorow"), ("so i", "so I ", "i")]
    )
    func undoCorrection(typed: String, corrected: String, word: String) throws {
        var session = TypingSession(corrector: try #require(TypingCorrector(language: "en-US")))
        var text = typed
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == corrected)
        text.apply(session.backspace(before: text))
        #expect(text == typed)
        text.apply(session.end(with: " ", before: text, corrects: true))
        text += "or " + word
        text.apply(session.end(with: ".", before: text, corrects: true))
        #expect(text == typed + " or " + word + ".")
    }

    @Test("once the cursor moves, backspace and a second space do only their plain job")
    func movedCursor() throws {
        var session = TypingSession(corrector: try #require(TypingCorrector(language: "en-US")))
        _ = session.end(with: " ", before: "see you tomorow", corrects: true)
        var elsewhere = "I said tomorrow "
        elsewhere.apply(session.backspace(before: elsewhere))
        #expect(elsewhere == "I said tomorrow")
        _ = session.end(with: " ", before: "hello", corrects: true)
        var otherField = "say "
        otherField.apply(session.end(with: " ", before: otherField, corrects: true))
        #expect(otherField == "say  ")
    }

    @Test("two shift taps within 0.35 seconds lock caps, but not with a letter typed between them")
    func shiftLock() {
        var session = TypingSession(corrector: nil)
        let start = Date()
        let first = session.tapShift(at: start)
        _ = session.type("u")
        let afterLetter = session.tapShift(at: start.addingTimeInterval(0.3))
        let quickSecond = session.tapShift(at: start.addingTimeInterval(0.5))
        #expect([first, afterLetter, quickSecond] == [false, false, true])
    }

    @Test("a second space after a word ends the sentence")
    func doubleSpace() {
        var session = TypingSession(corrector: nil)
        var text = "see you"
        text.apply(session.end(with: " ", before: text, corrects: true))
        text.apply(session.end(with: " ", before: text, corrects: true))
        #expect(text == "see you. ")
    }

    @Test("a tapped suggestion replaces the word and adds a space, which punctuation then replaces")
    func chooseSuggestion() {
        var session = TypingSession(corrector: nil)
        var text = "see you tomo"
        text.apply(session.choose("tomorrow", before: text))
        #expect(text == "see you tomorrow ")
        text.apply(session.end(with: "!", before: text, corrects: true))
        #expect(text == "see you tomorrow! ")
    }
}

private extension String {
    mutating func apply(_ edit: TypingSession.Edit) {
        removeLast(edit.deleteCount)
        append(edit.insert)
    }
}

import CoreGraphics
import Foundation
import Testing

/// Swipes drawn on the letter rows as an iPhone 17 Pro shows them in portrait: keys 40 points
/// wide and 58 tall.
@MainActor
struct SwipeTypingTests {
    /// Written before the decoder first ran; nothing was tuned on them.
    static let words = [
        "hello", "world", "thanks", "meeting", "tomorrow", "dinner", "great", "later", "coming", "people",
        "because", "really", "should", "something", "morning", "weekend", "together", "question", "minute",
        "place", "sorry", "happy", "little", "before", "where", "there", "about", "would", "could", "think",
        "going", "right", "time", "good", "call", "home", "work", "love", "night", "today",
    ]

    static func decoder(_ language: TypingLanguage) -> SwipeDecoder {
        SwipeDecoder(language: language, unit: 40.2, rowHeight: 58)
    }

    /// A thumb's path through `word`: each letter's key centre pushed off-centre by up to
    /// `wobble` key widths in a fixed pattern, joined by straight strokes sampled every few points.
    static func path(for word: String, on decoder: SwipeDecoder, wobble: CGFloat = 0.3) -> [CGPoint] {
        let stops = word.enumerated().compactMap { index, letter -> CGPoint? in
            guard let centre = decoder.centres[letter] else { return nil }
            let angle = CGFloat(index) * 2.4
            return CGPoint(
                x: centre.x + cos(angle) * wobble * decoder.keyWidth,
                y: centre.y + sin(angle) * wobble * decoder.keyWidth
            )
        }
        var path = [stops[0]]
        for (a, b) in zip(stops, stops.dropFirst()) {
            for step in 1...8 {
                let t = CGFloat(step) / 8
                path.append(CGPoint(x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t))
            }
        }
        return path
    }

    @Test("a swipe through a common word puts that word first at least 17 times in 20, and in the four offered 19 in 20")
    func commonWords() throws {
        let corrector = try #require(TypingCorrector(language: "en-US"))
        let decoder = Self.decoder(.english)
        var first = 0, offered = 0
        var misses: [String] = []
        for word in Self.words {
            let found = decoder.words(for: Self.path(for: word, on: decoder), dictionary: corrector.completions(of:), names: [])
            if found.first?.lowercased() == word { first += 1 } else { misses.append("\(word) -> \(found)") }
            if found.contains(where: { $0.lowercased() == word }) { offered += 1 }
        }
        #expect(first * 20 >= Self.words.count * 17, "\(misses)")
        #expect(offered * 20 >= Self.words.count * 19, "\(misses)")
    }

    @Test("a contact name the dictionary lacks is found by its shape, spelled as the contact has it")
    func contactName() throws {
        let corrector = try #require(TypingCorrector(language: "en-US"))
        let decoder = Self.decoder(.english)
        let found = decoder.words(for: Self.path(for: "dauren", on: decoder), dictionary: corrector.completions(of:), names: ["Dauren"])
        #expect(found.first == "Dauren", "\(found)")
    }

    @Test("a short drag inside one key is a tap, not a word")
    func shortDragIsNotASwipe() throws {
        let corrector = try #require(TypingCorrector(language: "en-US"))
        let decoder = Self.decoder(.english)
        let g = try #require(decoder.centres["g"])
        let found = decoder.words(for: [g, CGPoint(x: g.x + 10, y: g.y + 4)], dictionary: corrector.completions(of:), names: [])
        #expect(found.isEmpty)
    }

    @Test("a typed prefix of a Contacts name offers the name, capitalized as in Contacts")
    func contactCompletion() throws {
        var lexicon = TypingLexicon()
        lexicon.add(shortcut: "Dauren", text: "Dauren")
        let session = TypingSession(corrector: try #require(TypingCorrector(language: "en-US")), lexicon: lexicon)
        #expect(session.suggestions(before: "ask dau")?.others.first == "Dauren")
    }

    @Test("a second swiped word gets a space in front, and backspace takes a swiped word and its space")
    func swipedWordsInText() {
        var session = TypingSession(corrector: nil)
        var text = ""
        text.apply(session.swipe(["Hello"], before: text))
        text.apply(session.swipe(["world", "would"], before: text))
        #expect(text == "Hello world")
        #expect(session.suggestions(before: text)?.others == ["would"])
        text.apply(session.backspace(before: text))
        #expect(text == "Hello")
        text.apply(session.end(with: ",", before: text, corrects: true))
        text.apply(session.swipe(["there"], before: text))
        #expect(text == "Hello, there")
    }

    @Test("picking a runner-up swaps it for the swiped word, and one backspace still takes it")
    func swipeRunnerUp() {
        var session = TypingSession(corrector: nil)
        var text = "see you"
        text.apply(session.swipe(["tonight", "tomorrow"], before: text))
        text.apply(session.choose("tomorrow", before: text))
        #expect(text == "see you tomorrow")
        #expect(session.suggestions(before: text)?.others == ["tonight"])
        text.apply(session.backspace(before: text))
        #expect(text == "see you")
    }
}

private extension String {
    mutating func apply(_ edit: TypingSession.Edit) {
        removeLast(edit.deleteCount)
        append(edit.insert)
    }
}

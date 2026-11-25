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

    static func decoder(_ language: TypingLanguage, layout: EnglishKeyboardLayout = .qwerty,
                        origin: CGPoint = .zero, unit: CGFloat = 40.2) -> SwipeDecoder {
        let centres = language.letterGrid(englishLayout: layout).keyCentres.mapValues {
            CGPoint(x: origin.x + CGFloat($0.x) * unit, y: origin.y + (CGFloat($0.y) + 0.5) * 58)
        }
        return SwipeDecoder(language: language, centres: centres, keyWidth: unit)
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

    @Test("swiping the top-row Z and O on QWERTZ produces the English word zoo")
    func swipeUsesTheAlternateGrid() {
        let decoder = Self.decoder(.english, layout: .qwertz, origin: CGPoint(x: 35, y: 58), unit: 40)
        let path = [CGPoint(x: 255, y: 87), CGPoint(x: 375, y: 87)]

        #expect(decoder.words(for: path, dictionary: { _ in [] }, names: ["zoo", "you"]).first == "zoo")
    }

    @Test("a swipe through a common word puts that word first at least 17 times in 20, and in the four offered 19 in 20")
    func commonWords() async throws {
        let corrector = try #require(TypingCorrector(language: "en-US"))
        corrector.common = await CommonWords.list(for: .english)
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

    /* These words stand in for a Russian spelling lexicon. The decoder has to find "ёлка"
     * by its held-key prefix, from a path that starts on е. */
    static let russianWords = [
        "привет", "приветствие", "при", "спасибо", "спас", "ёлка", "ёж", "есть", "завтра", "завод",
        "дома", "дом", "домой", "хорошо", "хор", "ход",
    ]

    @Test(
        "on the Russian layout a swipe spells Russian words, ё included",
        arguments: ["привет", "спасибо", "ёлка", "завтра", "дома", "хорошо"]
    )
    func russian(word: String) {
        let decoder = Self.decoder(.russian)
        let plain = word.replacingOccurrences(of: "ё", with: "е")
        let dictionary = { (prefix: String) in Self.russianWords.filter { $0.hasPrefix(prefix) } }
        let found = decoder.words(for: Self.path(for: plain, on: decoder), dictionary: dictionary, names: [])
        #expect(found.first == word, "\(found)")
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

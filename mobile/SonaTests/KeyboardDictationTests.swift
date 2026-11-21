import Foundation
import Testing

/// The keyboard dictating by itself, driven by what the person and the microphone do.
struct KeyboardDictationTests {
    @Test("a recognizer that fails before any word hands dictation to the app, as a refused microphone does")
    func recognizerFailureFallsBack() {
        var dictation = KeyboardDictation()
        _ = dictation.handle(.start)
        _ = dictation.handle(.captureStarted)
        #expect(dictation.handle(.recognitionFailed) == [.abandon, .fallBack])
        #expect(dictation.handle(.start) == [.beginCapture])
    }

    @Test("a microphone that stops mid-word keeps every word the recognizer then finishes")
    func captureStoppedMidWordKeepsTheWords() {
        var dictation = KeyboardDictation()
        _ = dictation.handle(.start)
        _ = dictation.handle(.captureStarted)
        _ = dictation.handle(.heard("Meet me at the sta", final: false))
        #expect(dictation.handle(.captureEnded) == [.endCapture])
        #expect(dictation.handle(.heard("Meet me at the station.", final: true)) == [.endCapture, .insert("Meet me at the station.")])
    }

    @Test("a recognizer that never finishes after the microphone stops still inserts what was heard")
    func silentRecognizerInsertsWhatWasHeard() {
        var dictation = KeyboardDictation()
        _ = dictation.handle(.start)
        _ = dictation.handle(.captureStarted)
        _ = dictation.handle(.heard("Meet me at the sta", final: false))
        _ = dictation.handle(.captureEnded)
        #expect(dictation.handle(.finishTimedOut) == [.abandon, .insert("Meet me at the sta")])
    }

    @Test("letting go of Dictate before the microphone starts inserts nothing and leaves Dictate ready")
    func releaseBeforeAudioStarts() {
        var dictation = KeyboardDictation()
        _ = dictation.handle(.start)
        #expect(dictation.handle(.stop) == [.abandon])
        #expect(dictation.handle(.captureStarted) == [])
        #expect(dictation.handle(.heard("late", final: true)) == [])
        #expect(dictation.handle(.start) == [.beginCapture])
    }

    @Test("a dictation cleanup leaves empty puts nothing in the field, so no style writes its instructions back")
    func fillerOnlyDictationInsertsNothing() {
        #expect(KeyboardDictation.text(of: "Um.", profile: .empty, language: "en-US") == nil)
        #expect(KeyboardDictation.text(of: "um so it works", profile: .empty, language: "en-US") != nil)
    }

    @Test("a dictation styled after the keyboard moved to another field waits for Insert instead of typing there")
    func lateDictationInAnotherFieldWaitsForReview() {
        let heardFor = KeyboardDictation.FieldMark(id: UUID(), shown: 1, before: "Hi", traits: "0 0")
        var other = heardFor
        other.id = UUID()
        #expect(KeyboardDictation.landing(heardFor: heardFor, now: other, reviews: false) == .review)
        #expect(KeyboardDictation.landing(heardFor: heardFor, now: heardFor, reviews: false) == .write)
        #expect(KeyboardDictation.landing(heardFor: heardFor, now: heardFor, reviews: true) == .review)
    }

    @Test("in a host that names no fields, a dictation is typed unless something since says the field changed")
    func unnamedFieldIsTheSameUntilSomethingChanges() {
        let heardFor = KeyboardDictation.FieldMark(id: nil, shown: 1, before: "Hi", traits: "0 0")
        #expect(KeyboardDictation.landing(heardFor: heardFor, now: heardFor, reviews: false) == .write)
        var hidden = heardFor
        hidden.shown = 2
        #expect(KeyboardDictation.landing(heardFor: heardFor, now: hidden, reviews: false) == .review)
        var edited = heardFor
        edited.before = "Hi there"
        #expect(KeyboardDictation.landing(heardFor: heardFor, now: edited, reviews: false) == .review)
        var keys = heardFor
        keys.traits = "7 0"
        #expect(KeyboardDictation.landing(heardFor: heardFor, now: keys, reviews: false) == .review)
    }

    @Test("a dictation landing while another waits on review joins it below, never replaces it")
    func secondDictationJoinsTheReview() {
        #expect(KeyboardDictation.reviewing("Second.", after: "First.") == "First.\n\nSecond.")
        #expect(KeyboardDictation.reviewing("Only.", after: nil) == "Only.")
    }
}

/// Dictated or swiped text joined to what is before the cursor.
@MainActor
struct DictationJoiningTests {
    /// What the field holds after `text` is dropped in whole where the host reports `host`
    /// before the cursor, with the keyboard's own last edit remembered in `echo`.
    func joined(_ text: String, before host: String, after: String = "", echo: FieldEcho = FieldEcho()) -> String {
        let before = echo.joining(before: host, after: after)
        return before + TypingSession.joiningSpace(before: before) + text
    }

    @Test("dictated text gets one space after a word or punctuation, and none at the start, after a space, a newline or an opening bracket", arguments: [
        ("Call Dauren", "Call Dauren Meet me."),
        ("Call Dauren ", "Call Dauren Meet me."),
        ("Sure.", "Sure. Meet me."),
        ("", "Meet me."),
        ("Hi\n", "Hi\nMeet me."),
        ("Note (", "Note (Meet me."),
        ("Hel", "Hel Meet me."),
        ("à l'école", "à l'école Meet me."),
    ])
    func joinsWithOneSpace(host: String, expected: String) {
        #expect(joined("Meet me.", before: host) == expected)
    }

    /// A field, and a host that reports it at once or one keyboard edit late. The keyboard
    /// side does what KeyboardViewController does: remember each edit, read the field again
    /// right after it, and hear about changes the host announces.
    @MainActor
    struct Host {
        /// How many of the keyboard's edits the host reports late, however long a step takes;
        /// nil never updates.
        let lag: Int?
        var history: [String]
        var echo = FieldEcho()


        init(lag: Int?, text: String = "") {
            self.lag = lag
            history = [text]
        }

        var text: String { history[history.count - 1] }
        var shown: String { lag.map { history[max(0, history.count - 1 - $0)] } ?? history[0] }
        var behind: Bool { echo.behind(before: shown, after: "") }

        mutating func keyboard(_ edit: TypingSession.Edit) {
            echo.edited(before: shown, after: "", deleting: edit.deleteCount, inserting: edit.insert)
            history.append(String(text.dropLast(edit.deleteCount)) + edit.insert)
            echo.hostReported(before: shown, after: "", ownEdit: true)
        }

        mutating func dictate(_ words: String) {
            keyboard(TypingSession.Edit(insert: TypingSession.joiningSpace(before: echo.joining(before: shown, after: "")) + words))
        }

        /// The app changed the field and the host said so.
        mutating func app(sets text: String) {
            history = [text]
            echo.hostReported(before: shown, after: "", ownEdit: false)
        }

        /// The app changed the field without a word to the keyboard.
        mutating func appSilently(sets text: String) {
            history = [text]
        }

        enum Step: CustomStringConvertible {
            case key(String), delete, dictate(String)
            /// The host starts reporting this many edits late, or never again, and the keyboard
            /// reads the field again, as it does on its next key.
            case lag(Int?)
            case silently(String), announced(String), newField(String)
            /// A space, corrected as the keyboard would correct it.
            case space

            var description: String {
                switch self {
                case let .key(text): "key \(text)"
                case .delete: "delete"
                case let .dictate(words): "dictate \(words)"
                case let .lag(lag): "lag \(lag.map(String.init) ?? "never")"
                case let .silently(text): "silent \(text)"
                case let .announced(text): "announced \(text)"
                case let .newField(text): "new field \(text)"
                case .space: "space"
                }
            }
        }

        mutating func run(_ step: Step) {
            switch step {
            case let .key(text): keyboard(TypingSession.Edit(insert: text))
            case .delete: keyboard(TypingSession.Edit(deleteCount: 1))
            case let .dictate(words): dictate(words)
            case let .lag(lag):
                self = Host(lag: lag, history: history, echo: echo, session: session)
                echo.hostReported(before: shown, after: "", ownEdit: true)
            case .space:
                keyboard(session.end(with: " ", before: shown, corrects: true, hostBehind: behind))
            case let .silently(text): appSilently(sets: text)
            case let .announced(text): app(sets: text)
            case let .newField(text):
                echo.forget()
                history = [text]
            }
        }

        static let corrector = TypingCorrector(language: "en-US")
        var session = TypingSession(corrector: Host.corrector)

        private init(lag: Int?, history: [String], echo: FieldEcho, session: TypingSession) {
            self.lag = lag
            self.history = history
            self.echo = echo
            self.session = session
        }
    }

    struct Pattern: CustomTestStringConvertible, Sendable {
        let name: String
        let lag: Int?
        let start: String
        let steps: [Host.Step]
        /// Whether the keyboard treats the host as behind after each step.
        let behind: [Bool]
        let text: String
        var testDescription: String { name }
    }

    static let t = true, f = false
    static let patterns: [Pattern] = [
        Pattern(name: "in step", lag: 0, start: "I said beca", steps: [.key("u"), .key("s"), .key("e")], behind: [f, f, f], text: "I said becaus" + "e"),
        Pattern(name: "1 behind, burst", lag: 1, start: "I said beca", steps: [.key("u"), .key("s"), .key("e")], behind: [t, t, t], text: "I said because"),
        Pattern(name: "2 behind, burst", lag: 2, start: "I said beca", steps: [.key("u"), .key("s"), .key("e")], behind: [t, t, t], text: "I said because"),
        Pattern(name: "3 behind, burst", lag: 3, start: "I said beca", steps: [.key("u"), .key("s"), .key("e")], behind: [t, t, t], text: "I said because"),
        Pattern(name: "never updates, burst", lag: nil, start: "I said beca", steps: [.key("u"), .key("s"), .key("e")], behind: [t, t, t], text: "I said because"),
        Pattern(name: "catches up mid burst", lag: 3, start: "Call ", steps: [.key("D"), .key("a"), .key("u"), .lag(0), .key("r")], behind: [t, t, t, f, f], text: "Call Daur"),
        Pattern(
            name: "3 behind then 1 behind, dictating", lag: 3, start: "Call ",
            steps: [.key("D"), .key("a"), .key("u"), .key("r"), .lag(1), .dictate("école"), .dictate("Meet"), .lag(0)],
            behind: [t, t, t, t, t, t, t, f], text: "Call Daur école Meet"
        ),
        Pattern(name: "dictation in step", lag: 0, start: "Call ", steps: [.key("Dauren"), .dictate("école"), .dictate("Meet me.")], behind: [f, f, f], text: "Call Dauren école Meet me."),
        Pattern(name: "dictation 1 behind", lag: 1, start: "Call ", steps: [.key("Dauren"), .dictate("école"), .dictate("Meet me.")], behind: [t, t, t], text: "Call Dauren école Meet me."),
        Pattern(name: "dictation 2 behind", lag: 2, start: "Call ", steps: [.key("Dauren"), .dictate("école"), .dictate("Meet me.")], behind: [t, t, t], text: "Call Dauren école Meet me."),
        Pattern(name: "dictation 5 behind", lag: 5, start: "Call ", steps: [.key("Dauren"), .dictate("école"), .dictate("Meet me.")], behind: [t, t, t], text: "Call Dauren école Meet me."),
        Pattern(name: "dictation, never updates", lag: nil, start: "Call ", steps: [.key("Dauren"), .dictate("école"), .dictate("Meet me.")], behind: [t, t, t], text: "Call Dauren école Meet me."),
        Pattern(name: "back to the same text, then caught up", lag: 1, start: "I said teh", steps: [.key("x"), .delete, .lag(0)], behind: [t, t, f], text: "I said teh"),
        Pattern(name: "quiet clear, in step", lag: 0, start: "Note: ", steps: [.key("H"), .key("i"), .silently(""), .dictate("See you.")], behind: [f, f, f, f], text: "See you."),
        Pattern(name: "quiet clear, 1 behind", lag: 1, start: "Note: ", steps: [.key("H"), .key("i"), .silently(""), .dictate("See you.")], behind: [t, t, f, t], text: "See you."),
        Pattern(name: "quiet clear of a new field, 1 behind", lag: 1, start: "", steps: [.key("H"), .key("i"), .silently(""), .dictate("See you.")], behind: [t, t, f, t], text: "See you."),
        Pattern(name: "quiet clear, 2 behind", lag: 2, start: "Note: ", steps: [.key("H"), .key("i"), .silently(""), .dictate("See you.")], behind: [t, t, f, t], text: "See you."),
        Pattern(name: "quiet clear, 3 behind", lag: 3, start: "Note: ", steps: [.key("H"), .key("i"), .silently(""), .dictate("See you.")], behind: [t, t, f, t], text: "See you."),
        Pattern(name: "quiet clear, never updates", lag: nil, start: "Note: ", steps: [.key("H"), .key("i"), .silently(""), .dictate("See you.")], behind: [t, t, f, t], text: "See you."),
        Pattern(name: "cursor moved", lag: 1, start: "Call ", steps: [.key("Dauren"), .announced("Call "), .dictate("Meet")], behind: [t, f, t], text: "Call Meet"),
        Pattern(name: "field switch", lag: 1, start: "", steps: [.dictate("Meet me."), .newField(""), .dictate("Hello")], behind: [t, f, t], text: "Hello"),
        /* After the backspace the host, two behind, happens to show the real text, so it
         * reads as caught up for that moment; the next key finds it behind again. */
        Pattern(
            name: "typo fixed with backspace, 2 behind, then caught up", lag: 2, start: "I said te",
            steps: [.key("h"), .key("x"), .delete, .key(" "), .key("b"), .lag(0)],
            behind: [t, t, f, t, t, f], text: "I said teh b"
        ),
        Pattern(name: "dictating twice into a host 1 behind", lag: 1, start: "Call Dauren ", steps: [.dictate("école"), .dictate("Meet")], behind: [t, t], text: "Call Dauren école Meet"),
        Pattern(name: "dictating twice into a host 2 behind", lag: 2, start: "Call Dauren ", steps: [.dictate("école"), .dictate("Meet")], behind: [t, t], text: "Call Dauren école Meet"),
        Pattern(
            name: "typing a word, then a corrected space, 1 behind", lag: 1, start: "I said ",
            steps: [.key("b"), .key("e"), .key("c"), .key("a"), .key("u"), .key("s"), .key("e"), .space],
            behind: [t, t, t, t, t, t, t, t], text: "I said because "
        ),
        Pattern(
            name: "typing a word, then a corrected space, 2 behind", lag: 2, start: "I said ",
            steps: [.key("b"), .key("e"), .key("c"), .key("a"), .key("u"), .key("s"), .key("e"), .space],
            behind: [t, t, t, t, t, t, t, t], text: "I said because "
        ),
        Pattern(name: "return to earlier text after catching up", lag: 1, start: "Call ", steps: [.key("D"), .key("a"), .lag(0), .silently("Call D"), .dictate("x")], behind: [t, t, f, f, f], text: "Call D x"),
    ]

    @Test("the keyboard knows when the host is behind, and joins dictation right, across lag patterns", arguments: patterns)
    func lagPatterns(_ pattern: Pattern) throws {
        _ = try #require(Host.corrector)
        var host = Host(lag: pattern.lag, text: pattern.start)
        var behind: [Bool] = []
        for step in pattern.steps {
            host.run(step)
            behind.append(host.behind)
        }
        #expect(behind == pattern.behind, "\(pattern.steps)")
        #expect(host.text == pattern.text)
    }

    struct Random {
        var state: UInt64
        mutating func next(_ count: Int) -> Int {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return Int((state >> 33) % UInt64(count))
        }
    }

    /// Thousands of seeded sessions of keys, backspaces and dictation into hosts 0 to 5 edits
    /// behind or never updating, which stay that far behind through pauses unless they catch
    /// up, with quiet clears once the host caught up, announced changes and new fields.
    /// Before every keyboard edit: a host showing old text must read as behind, a host
    /// showing the real text must not, and dictation must join with the space the real text
    /// needs.
    @Test("across seeded random sessions the keyboard makes no edit on stale text, never stays careful once caught up, and joins dictation right", arguments: [1, 2, 3] as [UInt64])
    func seededSessions(seed: UInt64) {
        var random = Random(state: seed)
        var unsafe = 0, stuck = 0, wrongSpace = 0, edits = 0, dictations = 0, clears = 0
        for _ in 0..<2_000 {
            var echo = FieldEcho()
            var history = [["", "a", "Note: "][random.next(3)]]
            var shown = 0
            let lag = [0, 1, 2, 3, 5, 1_000][random.next(6)]
            for _ in 0..<12 {
                let real = history[history.count - 1]
                let op = random.next(11)
                if op <= 5 {
                    let before = history[shown]
                    echo.hostReported(before: before, after: "", ownEdit: true)
                    let behind = echo.behind(before: before, after: "")
                    edits += 1
                    if before != real, !behind { unsafe += 1 }
                    if before == real, behind { stuck += 1 }
                    var deleting = 0, inserting = ""
                    switch op {
                    case 0, 1: inserting = ["a", "b"][random.next(2)]
                    case 2: inserting = " "
                    case 3: deleting = 1
                    default:
                        let joined = echo.joining(before: before, after: "")
                        inserting = TypingSession.joiningSpace(before: joined) + "W"
                        dictations += 1
                        if TypingSession.joiningSpace(before: joined) != TypingSession.joiningSpace(before: real) { wrongSpace += 1 }
                    }
                    echo.edited(before: before, after: "", deleting: deleting, inserting: inserting)
                    history.append(String(real.dropLast(deleting)) + inserting)
                    let oldest = max(shown, history.count - 1 - lag)
                    shown = oldest + random.next(history.count - oldest)
                    echo.hostReported(before: history[shown], after: "", ownEdit: true)
                } else if op == 6 {
                    shown = history.count - 1
                    echo.hostReported(before: history[shown], after: "", ownEdit: true)
                } else if op == 7 {
                    shown = history.count - 1
                    echo.hostReported(before: history[shown], after: "", ownEdit: true)
                    /* The one case left: a quiet clear to text the memory still holds as an
                     * older version, as after a one-edit message, reads as the host behind.
                     * These clears land on text the session has not held before. */
                    clears += 1
                    history = ["Draft \(clears): "]
                    shown = 0
                } else if op == 8 {
                    history = [["", "a"][random.next(2)]]
                    shown = 0
                    echo.hostReported(before: history[0], after: "", ownEdit: false)
                } else if op == 9 {
                    history = [["", "a"][random.next(2)]]
                    shown = 0
                    echo.forget()
                } else {
                    if random.next(2) == 0 { shown = history.count - 1 }
                }
            }
        }
        #expect(edits > 10_000)
        #expect(dictations > 3_000)
        #expect(unsafe == 0, "unsafe edits \(unsafe) of \(edits)")
        #expect(stuck == 0, "stuck in plain mode \(stuck) of \(edits)")
        #expect(wrongSpace == 0, "wrong joining spaces \(wrongSpace) of \(dictations)")
        print("seed \(seed): \(edits) edits, \(dictations) dictations, unsafe \(unsafe), stuck \(stuck), wrong spaces \(wrongSpace)")
    }

    @Test("a host that never updates stays behind through a long burst")
    func neverUpdatesLongBurst() {
        var host = Host(lag: nil, text: "x")
        for _ in 0..<200 { host.keyboard(TypingSession.Edit(insert: "a")) }
        #expect(host.behind)
    }

    @Test("with a host two edits behind, or one that never updates, dictated words neither glue on nor double their space", arguments: [2, nil] as [Int?])
    func farBehindHostJoins(lag: Int?) {
        var host = Host(lag: lag, text: "Call ")
        host.keyboard(TypingSession.Edit(insert: "Dauren"))
        host.dictate("école")
        host.dictate("Meet me.")
        #expect(host.text == "Call Dauren école Meet me.")
    }

    @Test("with a host one edit behind, dictated words neither glue on nor double their space")
    func laggingHostJoins() {
        var host = Host(lag: 1, text: "Call Daur")
        host.keyboard(TypingSession.Edit(deleteCount: 4, insert: "Dauren "))
        host.dictate("école")
        host.dictate("Meet me.")
        #expect(host.text == "Call Dauren école Meet me.")
    }

    @Test("while the host is behind, even after two quick keys, a space does not correct the stale word it reports", arguments: [1, 2, nil] as [Int?])
    func laggingHostGetsNoCorrection(lag: Int?) throws {
        var session = TypingSession(corrector: try #require(TypingCorrector(language: "en-US")))
        var host = Host(lag: lag, text: "I said beca")
        host.keyboard(session.type("u"))
        host.keyboard(session.type("s"))
        #expect(host.behind)
        #expect(session.end(with: " ", before: host.shown, corrects: true, hostBehind: host.behind) == TypingSession.Edit(insert: " "))
    }

    @Test("a host that shows the edit ends the memory, so a field it clears without a word starts fresh")
    func silentClearAfterHostCaughtUp() {
        var host = Host(lag: 0)
        host.dictate("Meet me at noon.")
        host.appSilently(sets: "")
        host.dictate("See you.")
        #expect(host.text == "See you.")
    }

    @Test("a change the host announces, a cleared field or a moved cursor, ends the memory even while the host is behind")
    func announcedChangeEndsMemory() {
        var host = Host(lag: 1)
        host.dictate("Meet me at noon.")
        host.app(sets: "")
        host.dictate("See you.")
        #expect(host.text == "See you.")

        var moved = Host(lag: 1, text: "Call ")
        moved.keyboard(TypingSession.Edit(insert: "Dauren"))
        moved.app(sets: "Call")
        moved.dictate("Meet")
        #expect(moved.text == "Call Meet")
    }

    @Test("another empty field starts fresh")
    func switchingBetweenEmptyFields() {
        var host = Host(lag: 1)
        host.dictate("Meet me.")
        host.echo.forget()
        host.appSilently(sets: "")
        host.dictate("Hello")
        #expect(host.text == "Hello")
    }
}

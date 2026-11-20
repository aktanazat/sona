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

    @Test("a host one edit behind joins the next words to what the keyboard's last edit left")
    func hostOneEditBehind() {
        var echo = FieldEcho()
        echo.edited(before: "Call Daur", after: "", deleting: 4, inserting: "Dauren ")
        echo.observe(before: "Call Daur", after: "")
        #expect(joined("école", before: "Call Daur", after: "", echo: echo) == "Call Dauren école")
        echo.edited(before: "Call Daur", after: "", deleting: 0, inserting: "école")
        #expect(joined("Meet me.", before: "Call Daur", after: "", echo: echo) == "Call Dauren école Meet me.")
    }

    @Test("a host that catches up and then shows the old text again is believed")
    func hostChangesBackToOldText() {
        var echo = FieldEcho()
        echo.edited(before: "Call Daur", after: "", deleting: 4, inserting: "Dauren ")
        echo.observe(before: "Call Dauren ", after: "")
        #expect(joined("ok", before: "Call Daur", after: "", echo: echo) == "Call Daur ok")
    }

    @Test("moving the cursor back after a swipe joins to the text at the cursor, not the swipe")
    func cursorMovedAfterSwipe() {
        var echo = FieldEcho()
        echo.edited(before: "Call ", after: "", deleting: 0, inserting: "Dauren")
        #expect(joined("Meet", before: "Call ", after: "Dauren", echo: echo) == "Call Meet")
    }

    @Test("a sent message clears the field, and the next dictation starts it fresh")
    func sendClearsFieldThenDictate() {
        var echo = FieldEcho()
        echo.edited(before: "", after: "", deleting: 0, inserting: "Meet me at noon.")
        echo.forget()
        #expect(joined("See you.", before: "", after: "", echo: echo) == "See you.")
    }

    @Test("another empty field starts fresh")
    func switchingBetweenEmptyFields() {
        var echo = FieldEcho()
        echo.edited(before: "", after: "", deleting: 0, inserting: "Meet me.")
        echo.forget()
        #expect(joined("Hello", before: "", after: "", echo: echo) == "Hello")
    }
}

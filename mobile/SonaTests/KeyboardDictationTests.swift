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

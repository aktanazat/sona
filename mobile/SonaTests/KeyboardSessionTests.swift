import Foundation
import Testing

final class KeyboardSessionTests {
    private let directory: URL

    init() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("keyboard-session-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: directory)
    }

    @Test("the keyboard reads the words heard so far, newest last, never more than the limit")
    func heardWordsReachTheKeyboardBounded() throws {
        let spoken = (1...80).map { "word\($0)" }.joined(separator: " ")
        let app = KeyboardSessionStore(directory: directory)
        try app.publish(KeyboardSessionState(phase: .listening, heard: KeyboardSessionState.heard(spoken)))
        let heard = try #require(try KeyboardSessionStore(directory: directory).state().heard)
        #expect(heard.count <= KeyboardSessionState.heardLimit)
        #expect(heard.hasSuffix("word79 word80"))
        #expect(heard.hasPrefix("…word"), "the first word shown is whole: \(heard)")
    }

    @Test("a short dictation reaches the keyboard whole, and nothing heard reaches it as nothing")
    func shortDictation() {
        #expect(KeyboardSessionState.heard(" Meet at noon ") == "Meet at noon")
        #expect(KeyboardSessionState.heard("  ") == nil)
    }

    @Test("a start carries the keyboard's typing language to the app, and the app hears in that language")
    func startCarriesLanguage() throws {
        let keyboard = KeyboardSessionStore(directory: directory)
        try keyboard.send(KeyboardCommand(
            id: UUID(), action: .start, requestID: UUID(), documentID: nil, createdAt: Date(),
            language: TypingLanguage.russian.rawValue
        ))
        let command = try #require(try KeyboardSessionStore(directory: directory).takeCommand())
        #expect(DictationPreferences.recognitionLanguage(for: command.language).hasPrefix("ru"))
    }

    @Test("a quick hold-to-talk release withdraws an unread start instead of overwriting it with an orphaned stop")
    func quickReleaseWithdrawsUnreadStart() throws {
        let store = KeyboardSessionStore(directory: directory)
        let requestID = UUID()
        try store.send(KeyboardCommand(
            id: UUID(), action: .start, requestID: requestID, documentID: nil, createdAt: Date()
        ))
        #expect(try store.cancelPendingStart(requestID: UUID()) == false)
        #expect(try store.cancelPendingStart(requestID: requestID) == true)
        #expect(try store.takeCommand() == nil)
    }

    @Test("once the app takes a start command, cancelPendingStart returns false so stop is still sent")
    func cancelPendingStartAfterAppReadsStart() throws {
        let store = KeyboardSessionStore(directory: directory)
        let requestID = UUID()
        try store.send(KeyboardCommand(
            id: UUID(), action: .start, requestID: requestID, documentID: nil, createdAt: Date()
        ))
        _ = try #require(try store.takeCommand())
        #expect(try store.cancelPendingStart(requestID: requestID) == false)
    }
}

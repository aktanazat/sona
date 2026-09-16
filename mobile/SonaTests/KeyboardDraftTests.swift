import Foundation
import Testing

extension KeyboardDraftState {
    /// The draft when one is waiting, so a wrong state fails one `#require`.
    var waiting: KeyboardDraft? {
        if case .waiting(let draft) = self { return draft }
        return nil
    }
}

final class KeyboardDraftTests {
    private let directory: URL
    private let now = Date(timeIntervalSince1970: 1_700_000_000)

    init() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("keyboard-draft-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: directory)
    }

    @Test("An inserted draft cannot be inserted by a second keyboard instance")
    func insertionConsumesDraft() throws {
        let app = KeyboardDraftStore(directory: directory)
        try app.save(text: "Send the revised agenda.", now: now)
        let keyboard = KeyboardDraftStore(directory: directory)
        let preview = try #require(try keyboard.load(now: now).waiting)
        #expect(try keyboard.take(id: preview.id, now: now) == "Send the revised agenda.")
        let reopenedKeyboard = KeyboardDraftStore(directory: directory)
        #expect(throws: KeyboardDraftError.changed) {
            try reopenedKeyboard.take(id: preview.id, now: now)
        }
    }

    @Test("An old preview cannot consume a newer draft")
    func replacementRequiresAnotherPreview() throws {
        let store = KeyboardDraftStore(directory: directory)
        try store.save(text: "Old draft", now: now)
        let oldPreview = try #require(try store.load(now: now).waiting)
        try store.save(text: "Corrected draft", now: now)
        #expect(throws: KeyboardDraftError.changed) {
            try store.take(id: oldPreview.id, now: now)
        }
        #expect(try store.load(now: now).waiting?.text == "Corrected draft")
    }

    @Test("A draft past its life says so instead of looking like a first run")
    func expiredDraftIsReportedAsExpired() throws {
        let store = KeyboardDraftStore(directory: directory)
        try store.save(text: "Expired draft", now: now)
        #expect(try store.load(now: now.addingTimeInterval(600)) == .expired)
    }

    @Test("A draft refused for age does not come back when the clock does")
    func expiredDraftIsGoneForGood() throws {
        let store = KeyboardDraftStore(directory: directory)
        let draft = try store.save(text: "Expired draft", now: now)
        #expect(throws: KeyboardDraftError.expired) {
            try store.take(id: draft.id, now: now.addingTimeInterval(600))
        }
        #expect(try store.load(now: now) == .missing)
    }

    @Test("Bytes no draft can be read from leave nothing waiting, not a failure")
    func undecodableDraftIsReportedAsMissing() throws {
        let store = KeyboardDraftStore(directory: directory)
        try store.save(text: "Readable draft", now: now)
        let file = directory.appendingPathComponent(KeyboardDraftStore.fileName)
        try Data("truncated".utf8).write(to: file)
        #expect(try store.load(now: now) == .missing)
    }
}

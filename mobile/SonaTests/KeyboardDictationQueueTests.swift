import Foundation
import Testing

/// Dictations the keyboard inserted, waiting for Sona to keep them in Notes.
struct KeyboardDictationQueueTests {
    let queue: KeyboardDictationQueue

    init() throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("keyboard-queue-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        queue = KeyboardDictationQueue(folder: folder)
    }

    @Test("a queued dictation's audio survives the sweep for abandoned audio; audio nothing names does not")
    func sweepKeepsQueuedAudio() throws {
        let kept = try KeyboardAudioFile(in: queue)
        try kept.append(Data(count: 32_000))
        let record = KeyboardDictationRecord(
            id: UUID(), text: "Meet at noon.", date: Date(), language: "en-US", audio: try kept.finish()
        )
        try queue.enqueue(record)
        let abandoned = try KeyboardAudioFile(in: queue)
        _ = try abandoned.finish()

        try queue.removeOrphanedAudio(olderThan: 0, now: Date().addingTimeInterval(1))

        #expect(try queue.pending() == [record])
        #expect(record.audio?.durationMs == 1_000)
        #expect(FileManager.default.fileExists(atPath: queue.audioURL(kept.name).path))
        #expect(!FileManager.default.fileExists(atPath: queue.audioURL(abandoned.name).path))
    }

    func record(seconds: Int, at date: Date) throws -> KeyboardDictationRecord {
        let file = try KeyboardAudioFile(in: queue)
        try file.append(Data(count: seconds * 32_000))
        let record = KeyboardDictationRecord(id: UUID(), text: "said at \(date)", date: date, language: "en-US", audio: try file.finish())
        try queue.enqueue(record)
        return record
    }

    @Test("queued audio past the size cap or the age cap goes, oldest first, and every dictation's text stays")
    func trimDropsOldestAudioKeepsText() throws {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let week = try record(seconds: 1, at: now.addingTimeInterval(-8 * 24 * 3600))
        let older = try record(seconds: 2, at: now.addingTimeInterval(-60))
        let newer = try record(seconds: 2, at: now)

        try queue.trimAudio(maxBytes: 3 * 32_000, maxAge: 7 * 24 * 3600, now: now)

        let kept = try queue.pending()
        #expect(kept.map(\.text) == [week, older, newer].map(\.text))
        #expect(kept.map { $0.audio != nil } == [false, false, true])
        #expect(!FileManager.default.fileExists(atPath: queue.audioURL(try #require(older.audio).file).path))
        #expect(FileManager.default.fileExists(atPath: queue.audioURL(try #require(newer.audio).file).path))
    }

    @MainActor @Test("a dictation Sona already kept before a crash is not kept twice, and the queue lets go of each one only once it is kept")
    func importKeepsEachOnce() async throws {
        let libraryURL = queue.folder.appendingPathComponent("library")
        let persistence = DispatchQueue(label: "test.keyboard-import")
        let library = PhoneLibrary(directory: libraryURL, persistenceQueue: persistence)
        let first = try record(seconds: 1, at: Date(timeIntervalSince1970: 1_800_000_000))
        let second = try record(seconds: 1, at: Date(timeIntervalSince1970: 1_800_000_060))
        #expect(await library.deliverDictation(
            id: first.id, kind: .dictation, text: first.text, date: first.date, audio: nil,
            keyboardStore: nil, documentID: nil, removeSourceAfterSaving: false
        ).saving.value)

        let gate = DispatchSemaphore(value: 0)
        persistence.async { gate.wait() }
        let importing = Task { await library.importKeyboardDictations(from: queue) }
        let deadline = ContinuousClock.now + .seconds(2)
        while try queue.pending().count == 2, ContinuousClock.now < deadline { await Task.yield() }
        #expect(try queue.pending().map(\.id) == [second.id], "the second is still being kept behind the held disk")
        gate.signal()
        await importing.value

        #expect(try queue.pending().isEmpty)
        #expect(Set(library.notes.map(\.id)) == [first.id, second.id])
    }
}

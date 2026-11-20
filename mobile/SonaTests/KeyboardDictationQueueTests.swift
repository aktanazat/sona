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
}

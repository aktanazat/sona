import Dispatch
import Foundation
import Testing

private actor RewriteGate {
    private var waiting: CheckedContinuation<String, Never>?
    private var start: CheckedContinuation<Void, Never>?
    private var finish: CheckedContinuation<Bool, Never>?
    private var cancelled: Bool?

    func wait() async -> String {
        let value = await withCheckedContinuation { continuation in
            waiting = continuation
            start?.resume()
            start = nil
        }
        cancelled = Task.isCancelled
        finish?.resume(returning: Task.isCancelled)
        finish = nil
        return value
    }

    func started() async {
        if waiting != nil { return }
        await withCheckedContinuation { start = $0 }
    }

    func release(_ value: String) {
        waiting?.resume(returning: value)
        waiting = nil
    }

    func cancellationObserved() async -> Bool {
        if let cancelled { return cancelled }
        return await withCheckedContinuation { finish = $0 }
    }
}

final class DictationDeliveryTests {
    private let directory: URL

    init() throws {
        directory = FileManager.default.temporaryDirectory.appending(path: "dictation-delivery-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    deinit { try? FileManager.default.removeItem(at: directory) }

    @MainActor @Test("A held rewrite releases the raw draft at the controlled deadline and cancels generation")
    func heldRewrite() async {
        let generation = RewriteGate()
        let deadline = RewriteGate()
        let rewrite = BoundedRewrite()
        let result = Task {
            await rewrite.run(operation: { await generation.wait() }, waitForDeadline: { _ = await deadline.wait() })
        }
        await generation.started()
        await deadline.started()
        await deadline.release("deadline")
        #expect(await result.value == .fallback)
        await generation.release("late output")
        #expect(await generation.cancellationObserved())
        let nextDeadline = RewriteGate()
        #expect(await rewrite.run(operation: { "next draft" }, waitForDeadline: {
            try Task.checkCancellation()
            _ = await nextDeadline.wait()
        }) == .rewritten("next draft"))
        await nextDeadline.release("finished")
    }

    @MainActor @Test("Cancelling a held rewrite never delivers its late output")
    func cancelledRewrite() async {
        let generation = RewriteGate()
        let deadline = RewriteGate()
        let rewrite = BoundedRewrite()
        let result = Task {
            await rewrite.run(operation: { await generation.wait() }, waitForDeadline: { _ = await deadline.wait() })
        }
        await generation.started()
        await deadline.started()
        rewrite.cancel()
        #expect(await result.value == .cancelled)
        await generation.release("must not insert")
        #expect(await generation.cancellationObserved())
        await deadline.release("late deadline")
    }

    @MainActor @Test("The same-document keyboard draft survives restart before delayed history can save")
    func delayedHistory() async throws {
        let queue = DispatchQueue(label: "test.delayed-library")
        let libraryURL = directory.appending(path: "library")
        let library = PhoneLibrary(directory: libraryURL, persistenceQueue: queue)
        let gate = DispatchSemaphore(value: 0)
        queue.async { gate.wait() }
        defer { gate.signal() }
        let audioURL = directory.appending(path: "speech.pcm")
        let samples = Data(repeating: 1, count: 32000)
        try samples.write(to: audioURL)
        let store = KeyboardDraftStore(directory: directory)
        let document = UUID()
        let delivery = library.deliverDictation(
            kind: .dictation, text: "Friday is ready.", date: Date(),
            audio: CapturedAudio(url: audioURL, byteLength: samples.count, sha256: "", durationMs: 1000),
            keyboardStore: store, documentID: document, removeSourceAfterSaving: true
        )
        let draft = try #require(delivery.draft)
        let restarted = KeyboardDraftStore(directory: store.directory)
        #expect(try restarted.load() == .waiting(draft))
        #expect(draft.documentID == document)
        #expect(try restarted.take(id: draft.id) == "Friday is ready.")
        #expect(library.notes.isEmpty)
        #expect(FileManager.default.fileExists(atPath: audioURL.path))
        gate.signal()
        #expect(await delivery.saving.value)
        let reopened = PhoneLibrary(directory: libraryURL)
        let note = try #require(reopened.notes.first)
        let wave = try #require(reopened.audioURL(note))
        #expect(note.text == "Friday is ready.")
        #expect(Data(try Data(contentsOf: wave).dropFirst(44)) == samples)
        #expect(!FileManager.default.fileExists(atPath: audioURL.path))
    }

    @MainActor @Test("A failed history save preserves the source audio and the insertable draft")
    func failedHistory() async throws {
        let libraryURL = directory.appending(path: "library")
        let library = PhoneLibrary(directory: libraryURL)
        try FileManager.default.removeItem(at: libraryURL)
        try Data("not a directory".utf8).write(to: libraryURL)
        let audioURL = directory.appending(path: "speech.pcm")
        try Data(repeating: 1, count: 32).write(to: audioURL)
        let store = KeyboardDraftStore(directory: directory)
        let delivery = library.deliverDictation(
            kind: .dictation, text: "Keep these words.", date: Date(),
            audio: CapturedAudio(url: audioURL, byteLength: 32, sha256: "", durationMs: 1),
            keyboardStore: store, documentID: UUID(), removeSourceAfterSaving: true
        )
        #expect(await delivery.saving.value == false)
        let draft = try #require(delivery.draft)
        #expect(try store.take(id: draft.id) == "Keep these words.")
        #expect(FileManager.default.fileExists(atPath: audioURL.path))
        #expect(library.error != nil)
    }
}

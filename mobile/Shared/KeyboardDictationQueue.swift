import Foundation

/// A dictation the keyboard heard and inserted itself, waiting in the shared container
/// for Sona to keep it in Notes as it keeps its own dictations.
struct KeyboardDictationRecord: Codable, Equatable {
    struct Audio: Codable, Equatable {
        /// 16 kHz mono signed 16-bit little-endian samples, the format Sona keeps.
        var file: String
        var byteLength: Int
        var durationMs: Int64
    }

    var id: UUID
    var text: String
    var date: Date
    var language: String
    var audio: Audio?
}

struct KeyboardDictationQueue {
    static let folderName = "keyboard-dictations"
    let folder: URL

    static func shared() throws -> Self {
        let folder = try KeyboardDraftStore.shared().directory.appendingPathComponent(folderName, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return Self(folder: folder)
    }

    func audioURL(_ name: String) -> URL { folder.appendingPathComponent(name) }

    func enqueue(_ record: KeyboardDictationRecord) throws {
        try JSONEncoder().encode(record).write(
            to: folder.appendingPathComponent("\(record.id.uuidString).json"),
            options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
        )
    }

    func pending() throws -> [KeyboardDictationRecord] {
        try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
            .compactMap { try? JSONDecoder().decode(KeyboardDictationRecord.self, from: Data(contentsOf: $0)) }
            .sorted { $0.date < $1.date }
    }

    /// Sona may stay closed for days while the keyboard keeps dictating, so the audio
    /// waiting for it is held to `maxBytes` and `maxAge`: the oldest audio goes first, and
    /// its text stays queued.
    func trimAudio(maxBytes: Int, maxAge: TimeInterval, now: Date = Date()) throws {
        let records = try pending()
        var total = records.compactMap(\.audio).reduce(0) { $0 + $1.byteLength }
        for var record in records {
            guard let audio = record.audio,
                  total > maxBytes || now.timeIntervalSince(record.date) > maxAge else { continue }
            try? FileManager.default.removeItem(at: audioURL(audio.file))
            record.audio = nil
            try enqueue(record)
            total -= audio.byteLength
        }
    }

    /// Removes a kept record. Its audio is removed by whoever kept it.
    func remove(_ record: KeyboardDictationRecord) throws {
        try FileManager.default.removeItem(at: folder.appendingPathComponent("\(record.id.uuidString).json"))
    }

    /// Audio no record names and older than `age`: a dictation abandoned or a keyboard
    /// ended before it inserted anything.
    func removeOrphanedAudio(olderThan age: TimeInterval, now: Date = Date()) throws {
        let named = Set(try pending().compactMap { $0.audio?.file })
        for file in try FileManager.default.contentsOfDirectory(
            at: folder, includingPropertiesForKeys: [.contentModificationDateKey]
        ) where file.pathExtension == "pcm" && !named.contains(file.lastPathComponent) {
            let modified = try file.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate
            if now.timeIntervalSince(modified ?? now) > age { try FileManager.default.removeItem(at: file) }
        }
    }
}

/// The samples of one keyboard dictation, written to the queue's folder as they arrive, so
/// none of it is held in the keyboard's memory.
final class KeyboardAudioFile {
    let name: String
    private let handle: FileHandle
    private var byteLength = 0

    init(in queue: KeyboardDictationQueue) throws {
        name = "\(UUID().uuidString).pcm"
        let url = queue.audioURL(name)
        FileManager.default.createFile(atPath: url.path, contents: nil)
        handle = try FileHandle(forWritingTo: url)
    }

    func append(_ samples: Data) throws {
        try handle.write(contentsOf: samples)
        byteLength += samples.count
    }

    func finish() throws -> KeyboardDictationRecord.Audio {
        try handle.close()
        return KeyboardDictationRecord.Audio(
            file: name, byteLength: byteLength,
            durationMs: Int64(byteLength / 2) * 1000 / 16_000
        )
    }
}

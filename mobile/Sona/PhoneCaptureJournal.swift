import CryptoKit
import Foundation

struct PhoneCaptureJournal: Codable, Sendable {
    private static let metadataFile = "capture.json"
    private static let audioFile = "audio.pcm"
    private static let fileProtection = FileProtectionType.completeUntilFirstUserAuthentication
    private static let writeOptions: Data.WritingOptions = [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
    static let defaultRoot = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appending(path: "unfinished-recordings", directoryHint: .isDirectory)

    enum WatchSource: String, Codable, Sendable {
        case aac = "watch.aac"
        case wave = "watch.wav"
    }

    let id: UUID
    let recordedAtUtcMs: Int64
    var bookmarkOffsetsMs: [Int64] = []
    var watchSource: WatchSource? = nil

    static func receiveWatch(
        source: URL, format: WatchSource, recordedAtUtcMs: Int64, root: URL = defaultRoot
    ) throws -> Self {
        let journal = Self(id: UUID(), recordedAtUtcMs: recordedAtUtcMs, watchSource: format)
        try journal.begin(under: root)
        try FileManager.default.copyItem(
            at: source, to: journal.directory(under: root).appending(path: format.rawValue)
        )
        return journal
    }

    func directory(under root: URL) -> URL {
        root.appending(path: id.uuidString, directoryHint: .isDirectory)
    }

    func audioURL(under root: URL) -> URL {
        directory(under: root).appending(path: Self.audioFile)
    }

    func begin(under root: URL) throws {
        let files = FileManager.default
        try files.createDirectory(at: root, withIntermediateDirectories: true, attributes: [.protectionKey: Self.fileProtection])
        var backup = URLResourceValues()
        backup.isExcludedFromBackup = true
        var excluded = root
        try excluded.setResourceValues(backup)
        let folder = directory(under: root)
        try files.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.protectionKey: Self.fileProtection])
        do {
            try save(under: root)
            guard files.createFile(atPath: audioURL(under: root).path, contents: nil, attributes: [.protectionKey: Self.fileProtection]) else {
                throw CaptureError.writeFailed
            }
        } catch {
            try? files.removeItem(at: folder)
            throw error
        }
    }

    func save(under root: URL) throws {
        try JSONEncoder().encode(self).write(to: directory(under: root).appending(path: Self.metadataFile), options: Self.writeOptions)
    }

    static func pending(under root: URL) throws -> [URL] {
        let keys: Set<URLResourceKey> = [.isDirectoryKey, .isSymbolicLinkKey]
        let folders: [URL]
        do {
            folders = try FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: Array(keys))
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return []
        }
        return try folders.filter { url in
            guard UUID(uuidString: url.lastPathComponent) != nil else { return false }
            do {
                let values = try url.resourceValues(forKeys: keys)
                guard values.isDirectory == true, values.isSymbolicLink == false else { return false }
                _ = try url.appending(path: metadataFile).resourceValues(forKeys: [.isRegularFileKey])
                return true
            } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
                return false
            }
        }
    }

    static func load(from directory: URL) throws -> Self {
        let file = directory.appending(path: metadataFile)
        let metadata = try file.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
        guard metadata.isRegularFile == true, metadata.isSymbolicLink == false,
              let bytes = metadata.fileSize, bytes <= 1_048_576
        else { throw CaptureError.unsupportedFormat }
        let capture = try JSONDecoder().decode(Self.self, from: Data(contentsOf: file))
        guard capture.id.uuidString == directory.lastPathComponent else { throw CaptureError.unsupportedFormat }
        return capture
    }

    func recoverAudio(under root: URL) throws -> CapturedAudio? {
        if let watchSource {
            return try transcodeToCaptureFormat(
                source: directory(under: root).appending(path: watchSource.rawValue),
                destination: audioURL(under: root)
            )
        }
        let url = audioURL(under: root)
        let metadata: URLResourceValues
        do {
            metadata = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .fileSizeKey])
        } catch let error as CocoaError where error.code == .fileReadNoSuchFile {
            return nil
        }
        guard metadata.isRegularFile == true, metadata.isSymbolicLink == false, let byteCount = metadata.fileSize else {
            throw CaptureError.unsupportedFormat
        }
        let frameSize = RecordingAudioFormat.bytesPerFrame * RecordingAudioFormat.channels
        let length = byteCount - byteCount % frameSize
        guard length > 0 else { return nil }
        guard length <= Int(UInt32.max) - 36 else { throw CaptureError.unsupportedFormat }
        let file = try length == byteCount ? FileHandle(forReadingFrom: url) : FileHandle(forUpdating: url)
        defer { try? file.close() }
        if length != byteCount {
            try file.truncate(atOffset: UInt64(length))
            try file.seek(toOffset: 0)
        }
        var digest = SHA256()
        var remaining = length
        while remaining > 0 {
            guard let bytes = try file.read(upToCount: min(remaining, 64 * 1024)), !bytes.isEmpty else {
                throw CaptureError.writeFailed
            }
            digest.update(data: bytes)
            remaining -= bytes.count
        }
        return CapturedAudio(url: url, byteLength: length, sha256: Base64URL.encode(Data(digest.finalize())),
                             durationMs: Int64(length / frameSize) * 1000 / Int64(RecordingAudioFormat.sampleRateHz))
    }
}

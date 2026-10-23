import AVFoundation
import Dispatch
import Combine
import Foundation

struct PhoneNote: Codable, Identifiable, Equatable, Sendable {
    enum Kind: String, Codable, CaseIterable, Sendable { case note, dictation, meeting }
    let id: UUID
    let kind: Kind
    let date: Date
    var title: String
    var text: String
    var audioFile: String?
    var duration: TimeInterval?
}

private final class PhoneLibraryDisk: @unchecked Sendable {
    struct Snapshot: Sendable {
        let revision: UInt64
        let notes: [PhoneNote]
    }

    let directory: URL
    private let queue: DispatchQueue
    private var notes: [PhoneNote] = []
    private var revision: UInt64 = 0
    private var index: URL { directory.appending(path: "index.json") }

    init(directory: URL, queue: DispatchQueue) {
        self.directory = directory
        self.queue = queue
    }

    func load() throws -> Snapshot {
        try queue.sync {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            if FileManager.default.fileExists(atPath: index.path) {
                notes = try JSONDecoder().decode([PhoneNote].self, from: Data(contentsOf: index))
            }
            return Snapshot(revision: revision, notes: notes)
        }
    }

    func keep(_ note: PhoneNote, audio: CapturedAudio?) throws -> Snapshot {
        try queue.sync { try keepOnQueue(note, audio: audio) }
    }

    func keepAsync(_ note: PhoneNote, audio: CapturedAudio?) async throws -> Snapshot {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do { continuation.resume(returning: try self.keepOnQueue(note, audio: audio)) }
                catch { continuation.resume(throwing: error) }
            }
        }
    }

    func update(_ note: PhoneNote) throws -> Snapshot {
        try queue.sync { try save(notes.map { $0.id == note.id ? note : $0 }) }
    }

    func delete(_ note: PhoneNote) throws -> (Snapshot, Error?) {
        try queue.sync {
            let snapshot = try save(notes.filter { $0.id != note.id })
            do {
                if let file = note.audioFile { try FileManager.default.removeItem(at: directory.appending(path: file)) }
                return (snapshot, nil)
            } catch { return (snapshot, error) }
        }
    }

    private func keepOnQueue(_ note: PhoneNote, audio: CapturedAudio?) throws -> Snapshot {
        if let audio, let file = note.audioFile {
            try Self.writeWave(audio, to: directory.appending(path: file))
        }
        do { return try save([note] + notes) }
        catch {
            if let file = note.audioFile { try? FileManager.default.removeItem(at: directory.appending(path: file)) }
            throw error
        }
    }

    private func save(_ next: [PhoneNote]) throws -> Snapshot {
        try JSONEncoder().encode(next).write(to: index, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        notes = next
        revision += 1
        return Snapshot(revision: revision, notes: notes)
    }

    private static func writeWave(_ audio: CapturedAudio, to url: URL) throws {
        guard let length = UInt32(exactly: audio.byteLength), length <= UInt32.max - 36 else {
            throw CaptureError.unsupportedFormat
        }
        var header = Data("RIFF".utf8)
        func append<T: FixedWidthInteger>(_ value: T) {
            var little = value.littleEndian
            withUnsafeBytes(of: &little) { header.append(contentsOf: $0) }
        }
        append(length + 36)
        header.append(Data("WAVEfmt ".utf8))
        append(UInt32(16)); append(UInt16(1)); append(UInt16(1))
        append(UInt32(16000)); append(UInt32(32000)); append(UInt16(2)); append(UInt16(16))
        header.append(Data("data".utf8)); append(length)
        try header.write(to: url, options: .completeFileProtectionUntilFirstUserAuthentication)
        do {
            let source = try FileHandle(forReadingFrom: audio.url)
            defer { try? source.close() }
            let destination = try FileHandle(forWritingTo: url)
            defer { try? destination.close() }
            try destination.seekToEnd()
            while let bytes = try source.read(upToCount: 64 * 1024), !bytes.isEmpty {
                try destination.write(contentsOf: bytes)
            }
        } catch {
            try? FileManager.default.removeItem(at: url)
            throw error
        }
    }
}

@MainActor
final class PhoneLibrary: ObservableObject {
    struct DictationDelivery {
        let draft: KeyboardDraft?
        let saving: Task<Bool, Never>
    }

    @Published private(set) var notes: [PhoneNote] = []
    @Published var error: String?
    private let disk: PhoneLibraryDisk
    private var revision: UInt64 = 0

    init(
        directory: URL = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appending(path: "phone-library", directoryHint: .isDirectory),
        persistenceQueue: DispatchQueue = DispatchQueue(label: "com.aktanazat.sona.phone-library", qos: .utility)
    ) {
        disk = PhoneLibraryDisk(directory: directory, queue: persistenceQueue)
        do { apply(try disk.load()) } catch { self.error = error.localizedDescription }
    }

    func keep(kind: PhoneNote.Kind, title: String, text: String, date: Date = Date(), audio: CapturedAudio? = nil) throws {
        try apply(disk.keep(Self.note(kind: kind, title: title, text: text, date: date, audio: audio), audio: audio))
    }

    func deliverDictation(
        kind: PhoneNote.Kind, text: String, date: Date, audio: CapturedAudio?,
        keyboardStore: KeyboardDraftStore?, documentID: UUID?, removeSourceAfterSaving: Bool
    ) -> DictationDelivery {
        var draft: KeyboardDraft?
        var draftError: String?
        do { draft = try keyboardStore?.save(text: text, documentID: documentID) }
        catch { draftError = error.localizedDescription; self.error = draftError }
        let note = Self.note(kind: kind, title: String(text.prefix(80)), text: text, date: date, audio: audio)
        let saving = Task {
            do {
                if audio != nil || !text.isEmpty {
                    apply(try await disk.keepAsync(note, audio: audio))
                }
                if removeSourceAfterSaving, let audio { try FileManager.default.removeItem(at: audio.url) }
                if let draftError { self.error = draftError }
                return true
            } catch {
                self.error = error.localizedDescription
                return false
            }
        }
        return DictationDelivery(draft: draft, saving: saving)
    }

    func update(_ note: PhoneNote) throws { try apply(disk.update(note)) }

    func delete(_ note: PhoneNote) throws {
        let (snapshot, removalError) = try disk.delete(note)
        apply(snapshot)
        if let removalError { throw removalError }
    }

    func audioURL(_ note: PhoneNote) -> URL? {
        note.audioFile.map { disk.directory.appending(path: $0) }
    }

    private func apply(_ snapshot: PhoneLibraryDisk.Snapshot) {
        guard snapshot.revision >= revision else { return }
        revision = snapshot.revision
        notes = snapshot.notes
        error = nil
    }

    private static func note(kind: PhoneNote.Kind, title: String, text: String, date: Date, audio: CapturedAudio?) -> PhoneNote {
        let id = UUID()
        return PhoneNote(id: id, kind: kind, date: date, title: title, text: text,
                         audioFile: audio.map { _ in "\(id.uuidString).wav" },
                         duration: audio.map { Double($0.durationMs) / 1000 })
    }
}

@MainActor
final class NotePlayback: ObservableObject {
    @Published private(set) var playing = false
    @Published private(set) var position: TimeInterval = 0
    @Published private(set) var duration: TimeInterval = 0
    @Published var rate: Float = 1 { didSet { player?.rate = rate } }
    @Published var error: String?
    private var player: AVAudioPlayer?
    private var ticker: Task<Void, Never>?

    func load(_ url: URL) {
        do {
            player = try AVAudioPlayer(contentsOf: url)
            player?.enableRate = true
            duration = player?.duration ?? 0
        } catch { self.error = error.localizedDescription }
    }

    func toggle() {
        guard let player else { return }
        if player.isPlaying { stop(); return }
        do {
            try AVAudioSession.sharedInstance().setCategory(.playback)
            try AVAudioSession.sharedInstance().setActive(true)
            if player.currentTime >= player.duration { player.currentTime = 0 }
            player.rate = rate
            guard player.play() else { throw CaptureError.unsupportedFormat }
            playing = true
            ticker = Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .milliseconds(200))
                    guard !Task.isCancelled, let self, let player = self.player else { return }
                    position = player.currentTime
                    if !player.isPlaying { stop(); return }
                }
            }
        } catch { self.error = error.localizedDescription }
    }

    func seek(_ value: TimeInterval) { player?.currentTime = value; position = value }
    func stop() {
        player?.pause()
        playing = false
        ticker?.cancel()
        ticker = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }
}

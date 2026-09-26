import AVFoundation
import Combine
import Foundation

struct PhoneNote: Codable, Identifiable, Equatable {
    enum Kind: String, Codable, CaseIterable { case note, dictation, meeting }
    let id: UUID
    let kind: Kind
    let date: Date
    var title: String
    var text: String
    var audioFile: String?
    var duration: TimeInterval?
}

/// Local history is independent of the outbox: uploading never removes playback or notes.
@MainActor
final class PhoneLibrary: ObservableObject {
    @Published private(set) var notes: [PhoneNote] = []
    @Published var error: String?
    private let directory: URL
    private let index: URL

    init(directory: URL = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appending(path: "phone-library", directoryHint: .isDirectory)) {
        self.directory = directory
        index = directory.appending(path: "index.json")
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            if FileManager.default.fileExists(atPath: index.path) {
                notes = try JSONDecoder().decode([PhoneNote].self, from: Data(contentsOf: index))
            }
        } catch { self.error = error.localizedDescription }
    }

    func keep(kind: PhoneNote.Kind, title: String, text: String, date: Date = Date(), audio: CapturedAudio? = nil) throws {
        let id = UUID()
        let audioFile = audio.map { _ in "\(id.uuidString).wav" }
        if let audio, let audioFile { try Self.writeWave(audio, to: directory.appending(path: audioFile)) }
        let note = PhoneNote(id: id, kind: kind, date: date, title: title, text: text,
                             audioFile: audioFile, duration: audio.map { Double($0.durationMs) / 1000 })
        do { try save([note] + notes) }
        catch {
            if let audioFile { try? FileManager.default.removeItem(at: directory.appending(path: audioFile)) }
            throw error
        }
    }

    func update(_ note: PhoneNote) throws {
        try save(notes.map { $0.id == note.id ? note : $0 })
    }

    func delete(_ note: PhoneNote) throws {
        try save(notes.filter { $0.id != note.id })
        if let url = audioURL(note) { try FileManager.default.removeItem(at: url) }
    }

    func audioURL(_ note: PhoneNote) -> URL? {
        note.audioFile.map { directory.appending(path: $0) }
    }

    private func save(_ next: [PhoneNote]) throws {
        try JSONEncoder().encode(next).write(to: index, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        notes = next
        error = nil
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

import Foundation
import CoreFoundation

/// Only control messages and the words heard so far cross the extension boundary.
/// Microphone buffers stay in the app.
struct KeyboardSessionState: Codable, Equatable {
    enum Phase: String, Codable { case off, ready, listening, finishing, finished, failed }
    var phase: Phase = .off
    var expiresAt: Date = .distantPast
    var heartbeat: Date = .distantPast
    var requestID: UUID?
    var message: String?
    /// The running dictation's latest words, as `heard(_:)` trims them.
    var heard: String?

    func isWarm(at now: Date) -> Bool {
        let age = now.timeIntervalSince(heartbeat)
        return expiresAt > now && age >= -5 && age < 6 && phase != .off && phase != .failed
    }

    static let heardLimit = 160

    /// The end of `text` the keyboard shows while it is spoken: whole words, newest last,
    /// at most `heardLimit` characters however long the dictation runs, since every partial
    /// result rewrites the shared file.
    static func heard(_ text: String) -> String? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return nil }
        guard text.count > heardLimit else { return text }
        let tail = text.suffix(heardLimit - 1)
        let start = tail.firstIndex(where: \.isWhitespace).map(tail.index(after:)) ?? tail.startIndex
        return "…" + tail[start...]
    }
}

struct KeyboardCommand: Codable, Equatable {
    enum Action: String, Codable { case start, stop, cancel }
    let id: UUID
    let action: Action
    let requestID: UUID
    let documentID: UUID?
    let createdAt: Date
    /// The keyboard's typing language, which a start asks the recognizer to listen in.
    var language: String?
    /// The style one recording asked for, empty for none; nil keeps the chosen style.
    var style: String?

    func isFresh(at now: Date) -> Bool {
        let age = now.timeIntervalSince(createdAt)
        return age >= -5 && age < 15
    }
}

struct KeyboardSessionStore {
    let directory: URL
    static func shared() throws -> Self {
        Self(directory: try KeyboardDraftStore.shared().directory)
    }

    func state() throws -> KeyboardSessionState {
        try read(KeyboardSessionState.self, name: "keyboard-session.json") ?? KeyboardSessionState()
    }

    func publish(_ state: KeyboardSessionState) throws {
        try write(state, name: "keyboard-session.json")
        KeyboardSignal.post(.state)
    }

    func send(_ command: KeyboardCommand) throws {
        try write(command, name: "keyboard-command.json")
        KeyboardSignal.post(.command)
    }

    /// Withdraws an unread `.start` command for `requestID` so a quick hold-to-talk release
    /// does not overwrite it with an orphaned `.stop`. Returns false if the app already took it.
    func cancelPendingStart(requestID: UUID) throws -> Bool {
        try coordinated { folder in
            let file = folder.appendingPathComponent("keyboard-command.json")
            guard FileManager.default.fileExists(atPath: file.path) else { return false }
            let data = try Data(contentsOf: file)
            let command = try JSONDecoder().decode(KeyboardCommand.self, from: data)
            guard command.action == .start, command.requestID == requestID else { return false }
            try FileManager.default.removeItem(at: file)
            return true
        }
    }

    /// Taking under the container lock makes a delivered command a one-time request.
    func takeCommand(now: Date = Date()) throws -> KeyboardCommand? {
        try coordinated { folder in
            let file = folder.appendingPathComponent("keyboard-command.json")
            guard FileManager.default.fileExists(atPath: file.path) else { return nil }
            let data = try Data(contentsOf: file)
            try FileManager.default.removeItem(at: file)
            let command = try JSONDecoder().decode(KeyboardCommand.self, from: data)
            return command.isFresh(at: now) ? command : nil
        }
    }

    private func read<T: Decodable>(_ type: T.Type, name: String) throws -> T? {
        try coordinated { folder in
            let file = folder.appendingPathComponent(name)
            guard FileManager.default.fileExists(atPath: file.path) else { return nil }
            return try JSONDecoder().decode(type, from: Data(contentsOf: file))
        }
    }

    private func write<T: Encodable>(_ value: T, name: String) throws {
        try coordinated { folder in
            try JSONEncoder().encode(value).write(
                to: folder.appendingPathComponent(name),
                options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
            )
        }
    }

    private func coordinated<T>(_ operation: (URL) throws -> T) throws -> T {
        let coordinator = NSFileCoordinator(filePresenter: nil)
        var error: NSError?
        var result: Result<T, Error>?
        coordinator.coordinate(writingItemAt: directory, options: [], error: &error) { folder in
            result = Result { try operation(folder) }
        }
        if let error { throw error }
        guard let result else { throw KeyboardDraftError.unavailable }
        return try result.get()
    }
}

/// Darwin notifications wake the app or extension; the coordinated file is the authority.
final class KeyboardSignal {
    enum Channel: String {
        case command = "com.aktanazat.sona.keyboard.command"
        case state = "com.aktanazat.sona.keyboard.state"
    }
    private let channel: Channel
    private let receive: () -> Void

    init(_ channel: Channel, receive: @escaping () -> Void) {
        self.channel = channel
        self.receive = receive
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDarwinNotifyCenter(), Unmanaged.passUnretained(self).toOpaque(),
            { _, observer, _, _, _ in
                guard let observer else { return }
                let signal = Unmanaged<KeyboardSignal>.fromOpaque(observer).takeUnretainedValue()
                signal.receive()
            }, channel.rawValue as CFString, nil, .deliverImmediately
        )
    }

    deinit {
        CFNotificationCenterRemoveObserver(
            CFNotificationCenterGetDarwinNotifyCenter(), Unmanaged.passUnretained(self).toOpaque(),
            CFNotificationName(channel.rawValue as CFString), nil
        )
    }

    static func post(_ channel: Channel) {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(channel.rawValue as CFString), nil, nil, true
        )
    }
}

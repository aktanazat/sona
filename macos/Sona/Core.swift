import Darwin
import Foundation

/// Any JSON value, kept as the core sent it. A command's error arrives this
/// way: a plain string for most commands, an enum or object for the ones
/// with their own error type.
enum JSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let single = try decoder.singleValueContainer()
        if single.decodeNil() {
            self = .null
        } else if let bool = try? single.decode(Bool.self) {
            self = .bool(bool)
        } else if let number = try? single.decode(Double.self) {
            self = .number(number)
        } else if let string = try? single.decode(String.self) {
            self = .string(string)
        } else if let array = try? single.decode([JSONValue].self) {
            self = .array(array)
        } else {
            self = .object(try single.decode([String: JSONValue].self))
        }
    }

    func encode(to encoder: Encoder) throws {
        var single = encoder.singleValueContainer()
        switch self {
        case .null: try single.encodeNil()
        case let .bool(bool): try single.encode(bool)
        case let .number(number): try single.encode(number)
        case let .string(string): try single.encode(string)
        case let .array(array): try single.encode(array)
        case let .object(object): try single.encode(object)
        }
    }

    /// The string a reader can act on: the string itself, an enum's name,
    /// or the JSON text of anything else.
    var message: String {
        switch self {
        case let .string(string): string
        case .null: "the core gave no reason"
        default: (try? JSONEncoder().encode(self)).flatMap { String(data: $0, encoding: .utf8) } ?? "unreadable error"
        }
    }

    /// The value as one of the core's own error types.
    func decoded<T: Decodable>(as type: T.Type = T.self) -> T? {
        guard let data = try? JSONEncoder().encode(self) else { return nil }
        return try? Core.decoder.decode(type, from: data)
    }
}

/// Literals, so mixed params read as one dictionary:
/// `["id": 3, "title": "x", "tags": ["a"]] as [String: JSONValue]`.
extension JSONValue: ExpressibleByNilLiteral, ExpressibleByBooleanLiteral, ExpressibleByIntegerLiteral,
    ExpressibleByFloatLiteral, ExpressibleByStringLiteral, ExpressibleByArrayLiteral, ExpressibleByDictionaryLiteral {
    init(nilLiteral: ()) { self = .null }
    init(booleanLiteral value: Bool) { self = .bool(value) }
    init(integerLiteral value: Int) { self = .number(Double(value)) }
    init(floatLiteral value: Double) { self = .number(value) }
    init(stringLiteral value: String) { self = .string(value) }
    init(arrayLiteral elements: JSONValue...) { self = .array(elements) }
    init(dictionaryLiteral elements: (String, JSONValue)...) {
        self = .object(Dictionary(elements, uniquingKeysWith: { _, last in last }))
    }

    /// Any Encodable, as the value it serializes to. For a struct param.
    init<T: Encodable>(_ value: T) throws {
        self = try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value))
    }
}

/// What went wrong between the shell and the core.
enum CoreError: Error, LocalizedError {
    /// The core binary is neither bundled nor named by `SONA_CORE`.
    case missingBinary
    /// The socket never accepted within the launch window.
    case unreachable(String)
    /// The core answered the request with an error: the command's own error
    /// value, or the bridge's reason for refusing the request.
    case remote(JSONValue)
    /// The socket closed before the reply arrived.
    case closed

    var errorDescription: String? {
        switch self {
        case .missingBinary: "The Sona core is not installed with this app."
        case let .unreachable(reason): "The Sona core did not start: \(reason)"
        case let .remote(value): value.message
        case .closed: "The Sona core stopped."
        }
    }

    /// The remote error as one of the core's own types, when it is one.
    func remote<T: Decodable>(as type: T.Type = T.self) -> T? {
        if case let .remote(value) = self { value.decoded(as: type) } else { nil }
    }
}

/// The Rust core: one child process, one Unix socket, one JSON object per line.
///
/// A request is `{"id", "method", "params"}` and its reply repeats the `id`
/// with `result` or `error`. An event arrives without an `id`, as
/// `{"event", "payload"}`. Replies resolve the continuation waiting on that
/// id; events go to the observers of that name, on the main actor, in the
/// order the core sent them.
final class Core: @unchecked Sendable {
    /// The first look at a frame. `error` is present, possibly as `null`,
    /// exactly when the request failed: a command whose error type is `()`
    /// fails with `null`.
    private struct Head: Decodable {
        let id: UInt64?
        let event: String?
        let error: JSONValue?

        private enum Key: String, CodingKey { case id, event, error }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: Key.self)
            id = try container.decodeIfPresent(UInt64.self, forKey: .id)
            event = try container.decodeIfPresent(String.self, forKey: .event)
            // Not a ternary: `JSONValue` is expressible by a nil literal, so a
            // bare `nil` branch would read as `.null` and mark every reply failed.
            if container.contains(.error) {
                error = try container.decodeIfPresent(JSONValue.self, forKey: .error) ?? .null
            } else {
                error = nil
            }
        }
    }

    private struct Envelope<T: Decodable>: Decodable {
        let result: T
    }

    private struct EventFrame<P: Decodable>: Decodable {
        let payload: P
    }

    private struct Request<P: Encodable>: Encodable {
        let id: UInt64
        let method: String
        let params: P?
    }

    private struct NoParams: Encodable {}

    private static let launchWindow: TimeInterval = 30
    private static let retryPause: UInt32 = 200_000
    private static let exitWindow: TimeInterval = 3
    private static let exitPoll: UInt32 = 20_000
    /// The Rust structs use snake_case; the Swift mirrors use camelCase.
    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    private let process = Process()
    private let socketPath: String
    private let lock = NSLock()
    private var socket: Int32 = -1
    private var pending: [UInt64: CheckedContinuation<Data, Error>] = [:]
    private var nextID: UInt64 = 0
    private var observers: [String: [@MainActor (Data) -> Void]] = [:]

    init() {
        socketPath = NSTemporaryDirectory() + "sona-core-\(getpid()).sock"
    }

    /// Registers interest in one event. The handler gets the whole frame,
    /// which `payload` decodes. Observers of a name run in registration
    /// order; an observer is for the life of the app.
    func observe(_ name: String, _ handler: @escaping @MainActor (Data) -> Void) {
        lock.withLock { observers[name, default: []].append(handler) }
    }

    /// Spawns the core and connects to it. The socket is bound only once every
    /// manager is ready, so the connect loop is the readiness check.
    func start() async throws {
        let binary = try Self.binary()
        process.executableURL = binary
        process.currentDirectoryURL = binary.deletingLastPathComponent()
        process.arguments = ["--native-socket", socketPath, "--no-tray", "--start-hidden"]
        try process.run()
        let socket = try await Task.detached(priority: .userInitiated) { [socketPath, process] in
            try Self.connect(to: socketPath, while: process)
        }.value
        lock.withLock { self.socket = socket }
        let reader = Thread { [self] in read(socket) }
        reader.name = "sona-core-reader"
        reader.start()
    }

    /// Asks the core to exit and gives it a moment to do so; a core that
    /// does not answer is killed. `Process.waitUntilExit` would spin a nested
    /// run loop inside the app's own termination, so this polls instead.
    /// The socket file is the shell's to remove: the core never unlinks it.
    func shutdown() {
        let socket = lock.withLock { self.socket }
        if socket >= 0 {
            _ = send("{\"id\":0,\"method\":\"shutdown\"}\n", to: socket)
        }
        let deadline = Date().addingTimeInterval(Self.exitWindow)
        while process.isRunning, Date() < deadline {
            usleep(Self.exitPoll)
        }
        if process.isRunning {
            process.terminate()
        }
        unlink(socketPath)
    }

    func request(_ method: String) async throws {
        _ = try await exchange(method, NoParams?.none)
    }

    func request<P: Encodable>(_ method: String, _ params: P) async throws {
        _ = try await exchange(method, params)
    }

    func request<T: Decodable>(_ method: String) async throws -> T {
        try decode(try await exchange(method, NoParams?.none))
    }

    func request<T: Decodable, P: Encodable>(_ method: String, _ params: P) async throws -> T {
        try decode(try await exchange(method, params))
    }

    /// Decodes an event's payload. The line is the frame `onEvent` received.
    static func payload<P: Decodable>(_ line: Data) throws -> P {
        try decoder.decode(EventFrame<P>.self, from: line).payload
    }

    /// The core ships as `Contents/Helpers/SonaCore.app`, a bundle with its own
    /// identifier so LaunchServices never treats it as a second copy of the shell.
    private static func binary() throws -> URL {
        let helper = Bundle.main.bundleURL
            .appending(path: "Contents/Helpers/SonaCore.app/Contents/MacOS/sona-core")
        if FileManager.default.isExecutableFile(atPath: helper.path) {
            return helper
        }
        if let override = ProcessInfo.processInfo.environment["SONA_CORE"], !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
        throw CoreError.missingBinary
    }

    /// Connects, retrying until the core binds the socket, exits, or the
    /// launch window closes. Runs off the main thread.
    private static func connect(to path: String, while process: Process) throws -> Int32 {
        let deadline = Date().addingTimeInterval(launchWindow)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard path.utf8.count < capacity else {
            throw CoreError.unreachable("socket path is too long: \(path)")
        }
        withUnsafeMutablePointer(to: &address.sun_path) { sunPath in
            sunPath.withMemoryRebound(to: CChar.self, capacity: capacity) { buffer in
                _ = strlcpy(buffer, path, capacity)
            }
        }
        let length = socklen_t(MemoryLayout<sockaddr_un>.size)
        while true {
            let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
            guard fd >= 0 else {
                throw CoreError.unreachable(String(cString: strerror(errno)))
            }
            // A write after the core died must fail with EPIPE, not end the
            // shell with SIGPIPE.
            var noSigpipe: Int32 = 1
            setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigpipe, socklen_t(MemoryLayout<Int32>.size))
            let connected = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { generic in
                    Darwin.connect(fd, generic, length)
                }
            }
            if connected == 0 {
                return fd
            }
            let failure = String(cString: strerror(errno))
            close(fd)
            if !process.isRunning {
                throw CoreError.unreachable("the core exited with status \(process.terminationStatus)")
            }
            if Date() > deadline {
                throw CoreError.unreachable(failure)
            }
            usleep(retryPause)
        }
    }

    private func exchange<P: Encodable>(_ method: String, _ params: P?) async throws -> Data {
        let (id, socket) = lock.withLock { () -> (UInt64, Int32) in
            nextID += 1
            return (nextID, self.socket)
        }
        guard socket >= 0 else { throw CoreError.closed }
        var frame = try JSONEncoder().encode(Request(id: id, method: method, params: params))
        frame.append(UInt8(ascii: "\n"))
        return try await withCheckedThrowingContinuation { continuation in
            lock.withLock { pending[id] = continuation }
            if !send(frame, to: socket) {
                let waiting = lock.withLock { pending.removeValue(forKey: id) }
                waiting?.resume(throwing: CoreError.closed)
            }
        }
    }

    private func decode<T: Decodable>(_ line: Data) throws -> T {
        try Self.decoder.decode(Envelope<T>.self, from: line).result
    }

    private func send(_ text: String, to socket: Int32) -> Bool {
        send(Data(text.utf8), to: socket)
    }

    /// One writer at a time, so a reply never lands inside another frame.
    private func send(_ frame: Data, to socket: Int32) -> Bool {
        lock.withLock {
            frame.withUnsafeBytes { bytes -> Bool in
                var offset = 0
                while offset < bytes.count {
                    let written = Darwin.write(socket, bytes.baseAddress! + offset, bytes.count - offset)
                    if written <= 0 {
                        return false
                    }
                    offset += written
                }
                return true
            }
        }
    }

    /// The reader thread: whole lines out of the socket, each one a frame.
    private func read(_ socket: Int32) {
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        var carried = Data()
        while true {
            let count = Darwin.read(socket, &buffer, buffer.count)
            if count <= 0 {
                break
            }
            carried.append(contentsOf: buffer[0..<count])
            while let newline = carried.firstIndex(of: UInt8(ascii: "\n")) {
                let line = carried.subdata(in: carried.startIndex..<newline)
                carried.removeSubrange(carried.startIndex...newline)
                dispatch(line)
            }
        }
        let waiting = lock.withLock { () -> [CheckedContinuation<Data, Error>] in
            self.socket = -1
            let all = Array(pending.values)
            pending.removeAll()
            return all
        }
        for continuation in waiting {
            continuation.resume(throwing: CoreError.closed)
        }
        close(socket)
    }

    private func dispatch(_ line: Data) {
        guard let head = try? JSONDecoder().decode(Head.self, from: line) else {
            return
        }
        if let event = head.event {
            let handlers = lock.withLock { observers[event] ?? [] }
            if !handlers.isEmpty {
                // The main queue is FIFO, which keeps stream text in order;
                // a Task per frame would not promise that.
                DispatchQueue.main.async {
                    MainActor.assumeIsolated {
                        for handler in handlers {
                            handler(line)
                        }
                    }
                }
            }
            return
        }
        guard let id = head.id, let continuation = lock.withLock({ pending.removeValue(forKey: id) }) else {
            return
        }
        if let error = head.error {
            continuation.resume(throwing: CoreError.remote(error))
        } else {
            continuation.resume(returning: line)
        }
    }
}

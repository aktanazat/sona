import Network
import XCTest

/// The phone's outbox against a stand-in Worker on 127.0.0.1 that answers the three
/// upload calls as `cloudflare/sona-companion` does in the cases that matter here.
final class OutboxDrainTests: XCTestCase {
    /// Two triggers at once (launch and the network coming back, or a question sent while
    /// a pass runs) must not stage and send one item twice: the Worker refuses the second
    /// copy for good and the item never uploads (app-audit SONA-C3).
    func testTwoDrainsAtOnceSendEachItemOnce() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        let queue = try makeQueue(worker)
        _ = try await queue.enqueue(ask: ask("First question"), askedAtUtcMs: 1)
        _ = try await queue.enqueue(ask: ask("Second question"), askedAtUtcMs: 2)

        async let first: Void = queue.drain()
        async let second: Void = queue.drain()
        _ = await (first, second)

        XCTAssertEqual(worker.refusals, [])
        let left = await queue.items()
        XCTAssertEqual(left, [])
        XCTAssertEqual(worker.committed.count, 2)
    }

    /// An upload the Worker purged answers `upload_incomplete` to the same ids forever;
    /// the item starts over under new ids instead of retrying them (app-audit SONA-C4).
    func testPurgedUploadStartsOverUnderNewIds() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        worker.purgeFirstUpload = true
        let queue = try makeQueue(worker)
        _ = try await queue.enqueue(ask: ask("First question"), askedAtUtcMs: 1)

        await queue.drain()
        let restarted = await queue.items()
        XCTAssertEqual(restarted.map(\.parked), [false])
        await queue.drain()

        let left = await queue.items()
        XCTAssertEqual(left, [])
        XCTAssertEqual(worker.committed.count, 1)
        let committed = try XCTUnwrap(worker.committed.first)
        XCTAssertFalse(worker.purged.contains(committed))
    }

    private func makeQueue(_ worker: FakeWorker) throws -> UploadQueue {
        let root = FileManager.default.temporaryDirectory.appending(path: "outbox-\(UUID().uuidString)")
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        let credentials = VaultCredentials(
            endpoint: "http://127.0.0.1:\(worker.port)",
            vaultId: "vault_outbox",
            vaultRootBase64URL: Base64URL.encode(Data(repeating: 7, count: 32))
        )
        return UploadQueue(root: root, identity: .mint(), credentials: credentials)
    }

    private func ask(_ question: String) -> QueuedAsk {
        QueuedAsk(conversationId: "conversation", question: question, recentTurns: [])
    }
}

/// Idempotency keys replay their first body and refuse another, a chunk must match the
/// digest its plan named, and a purged upload answers `upload_incomplete`.
private final class FakeWorker: @unchecked Sendable {
    let port: UInt16
    var purgeFirstUpload: Bool {
        get { lock.withLock { purgesFirstUpload } }
        set { lock.withLock { purgesFirstUpload = newValue } }
    }
    var refusals: [String] { lock.withLock { refusalCodes } }
    var committed: [String] { lock.withLock { committedUploads } }
    var purged: Set<String> { lock.withLock { purgedUploads } }

    private let listener: NWListener
    private let lock = NSLock()
    private var chunkDigests: [String: [Int: String]] = [:]
    private var keyedBodies: [String: String] = [:]
    private var purgedUploads: Set<String> = []
    private var committedUploads: [String] = []
    private var refusalCodes: [String] = []
    private var uploadsSeen = 0
    private var purgesFirstUpload = false

    private init(listener: NWListener, port: UInt16) {
        self.listener = listener
        self.port = port
    }

    static func start() async throws -> FakeWorker {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        let listener = try NWListener(using: parameters)
        let queue = DispatchQueue(label: "fake-worker")
        let port: UInt16 = try await withCheckedThrowingContinuation { continuation in
            listener.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    listener.stateUpdateHandler = nil
                    continuation.resume(returning: listener.port?.rawValue ?? 0)
                case let .failed(error):
                    listener.stateUpdateHandler = nil
                    continuation.resume(throwing: error)
                default:
                    break
                }
            }
            listener.newConnectionHandler = { _ in }
            listener.start(queue: queue)
        }
        let worker = FakeWorker(listener: listener, port: port)
        listener.newConnectionHandler = { [worker] connection in
            connection.start(queue: queue)
            worker.read(connection, buffer: Data())
        }
        return worker
    }

    func stop() { listener.cancel() }

    private func read(_ connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 20) { [self] data, _, done, error in
            var buffer = buffer
            if let data { buffer.append(data) }
            if let request = Request(buffer) {
                let (status, body) = answer(request)
                /* A Worker answer takes a little while; the delay opens the window two
                 * passes would share. */
                DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(30)) {
                    connection.send(content: Self.response(status, body), completion: .contentProcessed { _ in
                        connection.cancel()
                    })
                }
            } else if error == nil, !done {
                read(connection, buffer: buffer)
            } else {
                connection.cancel()
            }
        }
    }

    private func answer(_ request: Request) -> (Int, String) {
        lock.withLock {
            let path = request.path.split(separator: "/").map(String.init)
            if let key = request.idempotencyKey {
                let digest = sha256Base64URL(request.body)
                if let stored = keyedBodies[key], stored != digest {
                    return refuse(409, "idempotency_conflict")
                }
                keyedBodies[key] = digest
            }
            let uploadId = path.count > 2 ? path[2] : ""
            switch (request.method, path.count, path.last ?? "") {
            case ("POST", 2, "uploads"):
                guard let plan = try? JSONDecoder().decode(Plan.self, from: request.body)
                else { return refuse(400, "invalid_request") }
                let uploadId = plan.uploadId
                if chunkDigests[uploadId] == nil {
                    uploadsSeen += 1
                    if purgesFirstUpload, uploadsSeen == 1 { purgedUploads.insert(uploadId) }
                }
                chunkDigests[uploadId] = Dictionary(uniqueKeysWithValues: plan.chunks.map { ($0.index, $0.sha256) })
                return (201, #"{"upload_id":"\#(uploadId)","state":"active","accepted_indexes":[]}"#)
            case let ("PUT", 5, index):
                if purgedUploads.contains(uploadId) { return refuse(409, "upload_incomplete") }
                guard let index = Int(index), chunkDigests[uploadId]?[index] == sha256Base64URL(request.body)
                else { return refuse(422, "integrity_failed") }
                return (200, #"{"upload_id":"\#(uploadId)","index":\#(index),"accepted":true}"#)
            case ("POST", 4, "commit"):
                if purgedUploads.contains(uploadId) { return refuse(409, "upload_incomplete") }
                if !committedUploads.contains(uploadId) { committedUploads.append(uploadId) }
                return (200, #"{"upload_id":"\#(uploadId)","state":"committed","revision_id":"revision"}"#)
            default:
                return refuse(404, "not_found")
            }
        }
    }

    private func refuse(_ status: Int, _ code: String) -> (Int, String) {
        refusalCodes.append(code)
        return (status, #"{"code":"\#(code)","request_id":"request","retryable":false}"#)
    }

    private static func response(_ status: Int, _ body: String) -> Data {
        let bytes = Data(body.utf8)
        let head = "HTTP/1.1 \(status) Status\r\nContent-Type: application/json\r\n"
            + "Content-Length: \(bytes.count)\r\nConnection: close\r\n\r\n"
        return Data(head.utf8) + bytes
    }

    /// The part of an upload plan the stand-in checks; the phone sends camelCase keys.
    private struct Plan: Decodable {
        struct Chunk: Decodable {
            let index: Int
            let sha256: String
        }

        let uploadId: String
        let chunks: [Chunk]
    }

    private struct Request {
        let method: String
        let path: String
        let idempotencyKey: String?
        let body: Data

        /// Nil until the whole request has arrived.
        init?(_ buffer: Data) {
            guard let end = buffer.range(of: Data("\r\n\r\n".utf8)),
                  let head = String(data: buffer[..<end.lowerBound], encoding: .utf8)
            else { return nil }
            let lines = head.components(separatedBy: "\r\n")
            let start = lines[0].split(separator: " ")
            guard start.count >= 2 else { return nil }
            var headers: [String: String] = [:]
            for line in lines.dropFirst() {
                guard let colon = line.firstIndex(of: ":") else { continue }
                headers[line[..<colon].lowercased()] = line[line.index(after: colon)...]
                    .trimmingCharacters(in: .whitespaces)
            }
            let length = Int(headers["content-length"] ?? "0") ?? 0
            guard buffer.count - end.upperBound >= length else { return nil }
            method = String(start[0])
            path = String(start[1].split(separator: "?")[0])
            idempotencyKey = headers["x-sona-idempotency-key"]
            body = Data(buffer[end.upperBound..<(end.upperBound + length)])
        }
    }
}

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

    /// A question asked while a long upload is in flight goes right after it, ahead of
    /// older items the pass already found, so it is not counted as not sent.
    func testQuestionAskedDuringALongUploadGoesNext() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        worker.holdFirstChunk = true
        let queue = try makeQueue(worker)
        let long = try await queue.enqueue(thought: thought("Long"), capturedAtUtcMs: 1, files: [])
        let older = try await queue.enqueue(thought: thought("Older"), capturedAtUtcMs: 2, files: [])

        let pass = Task { await queue.drain() }
        try await worker.waitUntilHolding()
        let question = try await queue.enqueue(ask: ask("Asked meanwhile"), askedAtUtcMs: 3)
        worker.release()
        await pass.value

        XCTAssertEqual(worker.committedObjects, [long.objectId, question.objectId, older.objectId])
    }

    /// An item being sent cannot be withdrawn: its commit may already be on the way.
    /// Anything else can, and then never reaches the Worker.
    func testWithdrawRefusesTheItemBeingSent() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        worker.holdFirstChunk = true
        let queue = try makeQueue(worker)
        let sending = try await queue.enqueue(ask: ask("First question"), askedAtUtcMs: 1)
        let waiting = try await queue.enqueue(ask: ask("Second question"), askedAtUtcMs: 2)

        let pass = Task { await queue.drain() }
        try await worker.waitUntilHolding()
        let refused = await queue.withdraw(objectId: sending.objectId)
        let taken = await queue.withdraw(objectId: waiting.objectId)
        worker.release()
        await pass.value

        XCTAssertFalse(refused)
        XCTAssertTrue(taken)
        XCTAssertEqual(worker.committedObjects, [sending.objectId])
        let left = await queue.items()
        XCTAssertEqual(left, [])
    }

    /// While another item's upload is held, longer than any window, a queued question
    /// reads as busy, so its clock does not run; once the outbox is idle it reads as
    /// waiting, and it leaves on the next pass.
    func testAQuestionBehindAHeldUploadReadsAsBusy() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        worker.holdFirstChunk = true
        let queue = try makeQueue(worker)
        _ = try await queue.enqueue(thought: thought("Long"), capturedAtUtcMs: 1, files: [])

        let pass = Task { await queue.drain() }
        try await worker.waitUntilHolding()
        let question = try await queue.enqueue(ask: ask("Asked meanwhile"), askedAtUtcMs: 2)
        let held = await queue.state(of: question.objectId)
        XCTAssertEqual(held, .busy)
        worker.release()
        await pass.value

        let after = await queue.state(of: question.objectId)
        XCTAssertEqual(after, .left)
    }

    /// A connection dropped after the create: the retry later sends the create again
    /// under the same key, so it must be the same bytes. Signing the plan again (CryptoKit
    /// signatures are randomized) made the Worker refuse it as an idempotency conflict.
    func testCreateRetriedAfterADroppedConnectionIsTheSameRequest() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        worker.dropFirstChunk = true
        let root = FileManager.default.temporaryDirectory.appending(path: "outbox-\(UUID().uuidString)")
        let queue = try makeQueue(worker, root: root)
        let item = try await queue.enqueue(ask: ask("Dropped once"), askedAtUtcMs: 1)
        await queue.drain()

        /* The retry the dropped connection scheduled comes due. */
        let file = root.appending(path: item.objectId).appending(path: "item.json")
        var stored = try JSONDecoder().decode(QueuedObject.self, from: Data(contentsOf: file))
        stored.nextAttemptUtcMs = 0
        try JSONEncoder().encode(stored).write(to: file)
        await queue.drain()

        XCTAssertEqual(worker.refusals, [])
        XCTAssertEqual(worker.committedObjects, [item.objectId])
    }

    /// A thought with several photos sends its chunks side by side, a few at a time, so
    /// it does not wait one round trip per chunk.
    func testChunksGoSeveralAtATime() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        let queue = try makeQueue(worker)
        var files: [URL] = []
        var attachments: [ThoughtAttachment] = []
        for index in 0..<6 {
            let bytes = Data(repeating: UInt8(index), count: 1024)
            let file = FileManager.default.temporaryDirectory.appending(path: "photo-\(UUID().uuidString).jpg")
            try bytes.write(to: file)
            files.append(file)
            attachments.append(ThoughtAttachment(
                kind: .image(mime: "image/jpeg", width: 32, height: 32), file: "image-\(index).jpg",
                byteLength: bytes.count, sha256: sha256Base64URL(bytes)
            ))
        }
        let photos = QueuedThought(origin: .typed, text: "Photos", link: nil, attachments: attachments)
        let item = try await queue.enqueue(thought: photos, capturedAtUtcMs: 1, files: files)

        await queue.drain()

        XCTAssertEqual(worker.refusals, [])
        XCTAssertEqual(worker.committedObjects, [item.objectId])
        XCTAssertGreaterThan(worker.mostChunksInFlight, 1)
        XCTAssertLessThanOrEqual(worker.mostChunksInFlight, 3)
    }

    /// A recording queued after its capture was encoded uploads the encoded bytes, and
    /// its manifest names their codec so the Mac picks the right decoder.
    func testEncodedRecordingUploadsTheEncodedBytesUnderItsCodec() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        worker.holdFirstChunk = true
        let root = FileManager.default.temporaryDirectory.appending(path: "outbox-\(UUID().uuidString)")
        let queue = try makeQueue(worker, root: root)
        let capture = try writeSpeechLikeCapture(seconds: 3)
        let upload = try encodeForUpload(
            capture, destination: capture.url.deletingPathExtension().appendingPathExtension("aac")
        )
        let encoded = try Data(contentsOf: upload.url)
        let item = try await queue.enqueue(audio: upload, recordedAtUtcMs: 1, title: "Encoded")

        let pass = Task { await queue.drain() }
        try await worker.waitUntilHolding()
        let manifest = try JSONDecoder().decode(
            DeviceRecordingManifest.self,
            from: stagedPlaintext(root, item, file: "manifest.bin", index: 0, kind: .manifest)
        )
        let chunk = try stagedPlaintext(root, item, file: "chunk-000000.bin", index: 0, kind: .chunk)
        worker.release()
        await pass.value

        XCTAssertEqual(manifest.audio.codec, "aac_adts")
        XCTAssertEqual(manifest.audio.byte_length, encoded.count)
        XCTAssertEqual(manifest.audio.sha256, sha256Base64URL(encoded))
        XCTAssertEqual(chunk, encoded)
        XCTAssertEqual(worker.refusals, [])
        XCTAssertEqual(worker.committedObjects, [item.objectId])
    }

    /// A recording the previous app version queued has no codec and only its PCM file;
    /// after the update it still uploads that PCM, named as PCM.
    func testRecordingQueuedBeforeTheUpdateStillUploadsItsPcm() async throws {
        let worker = try await FakeWorker.start()
        defer { worker.stop() }
        worker.holdFirstChunk = true
        let root = FileManager.default.temporaryDirectory.appending(path: "outbox-\(UUID().uuidString)")
        let queue = try makeQueue(worker, root: root)
        let capture = try writeSpeechLikeCapture(seconds: 1)
        let pcm = try Data(contentsOf: capture.url)
        let item = QueuedObject(
            objectId: randomOpaqueId(), revisionId: randomOpaqueId(), uploadId: randomOpaqueId(),
            capturedAtUtcMs: 1,
            payload: .recording(QueuedRecording(
                durationMs: capture.durationMs, title: "Queued before", audioByteLength: pcm.count,
                audioSha256: capture.sha256
            )),
            stagedForVaultId: nil, chunkSizes: [], chunkDigests: [], attempts: 0,
            nextAttemptUtcMs: 0, lastError: nil, parked: false
        )
        let itemJson = try JSONEncoder().encode(item)
        XCTAssertFalse(String(decoding: itemJson, as: UTF8.self).contains("audioCodec"),
                       "an item without a codec is byte for byte what the previous version wrote")
        let directory = root.appending(path: item.objectId)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try itemJson.write(to: directory.appending(path: "item.json"))
        try FileManager.default.moveItem(at: capture.url, to: directory.appending(path: "audio.pcm"))

        let pass = Task { await queue.drain() }
        try await worker.waitUntilHolding()
        let manifest = try JSONDecoder().decode(
            DeviceRecordingManifest.self,
            from: stagedPlaintext(root, item, file: "manifest.bin", index: 0, kind: .manifest)
        )
        let chunk = try stagedPlaintext(root, item, file: "chunk-000000.bin", index: 0, kind: .chunk)
        worker.release()
        await pass.value

        XCTAssertEqual(manifest.audio.codec, "pcm_s16le")
        XCTAssertEqual(manifest.audio.byte_length, pcm.count)
        XCTAssertEqual(chunk, pcm)
        XCTAssertEqual(worker.committedObjects, [item.objectId])
    }

    /// One staged payload of a single-chunk recording, opened as the Mac opens it.
    private func stagedPlaintext(
        _ root: URL, _ item: QueuedObject, file: String, index: UInt64, kind: ObjectContentKind
    ) throws -> Data {
        try openObjectRevisionPayload(
            vaultRoot: Data(repeating: 7, count: 32),
            context: ObjectRevisionCryptoContext(
                vaultId: "vault_outbox", objectId: item.objectId, revisionId: item.revisionId,
                index: index, total: 1, contentKind: kind,
                sourceFormat: DeviceRecordingObject.sourceFormat
            ),
            encryptedPayload: Data(contentsOf: root.appending(path: item.objectId).appending(path: file))
        )
    }

    private func makeQueue(
        _ worker: FakeWorker,
        root: URL = FileManager.default.temporaryDirectory.appending(path: "outbox-\(UUID().uuidString)")
    ) throws -> UploadQueue {
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

    private func thought(_ text: String) -> QueuedThought {
        QueuedThought(origin: .typed, text: text, link: nil, attachments: [])
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
    /// Object ids in the order their uploads committed.
    var committedObjects: [String] { lock.withLock { committedUploads.compactMap { objectOf[$0] } } }
    /// The first chunk's answer waits for `release()`, so a test can act while an upload
    /// is in flight.
    var holdFirstChunk: Bool {
        get { lock.withLock { holdsFirstChunk } }
        set { lock.withLock { holdsFirstChunk = newValue } }
    }
    var isHolding: Bool { lock.withLock { held != nil } }
    /// The most chunk uploads the stand-in held unanswered at once.
    var mostChunksInFlight: Int { lock.withLock { mostChunks } }
    /// The first chunk upload is cut off before an answer, as a dropped connection does.
    var dropFirstChunk: Bool {
        get { lock.withLock { dropsFirstChunk } }
        set { lock.withLock { dropsFirstChunk = newValue } }
    }

    private let listener: NWListener
    private let lock = NSLock()
    private var chunkDigests: [String: [Int: String]] = [:]
    private var keyedBodies: [String: String] = [:]
    private var purgedUploads: Set<String> = []
    private var committedUploads: [String] = []
    private var refusalCodes: [String] = []
    private var uploadsSeen = 0
    private var purgesFirstUpload = false
    private var objectOf: [String: String] = [:]
    private var holdsFirstChunk = false
    private var dropsFirstChunk = false
    private var held: (() -> Void)?
    private var chunksInFlight = 0
    private var mostChunks = 0

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

    func release() {
        let respond = lock.withLock {
            defer { held = nil }
            return held
        }
        respond?()
    }

    /// Waits until the held chunk has arrived.
    func waitUntilHolding(within seconds: Double = 5) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !isHolding {
            guard Date() < deadline else { throw CancellationError() }
            try await Task.sleep(for: .milliseconds(10))
        }
    }

    private func read(_ connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 20) { [self] data, _, done, error in
            var buffer = buffer
            if let data { buffer.append(data) }
            if let request = Request(buffer) {
                let drop = lock.withLock {
                    guard request.method == "PUT", dropsFirstChunk else { return false }
                    dropsFirstChunk = false
                    return true
                }
                if drop {
                    connection.cancel()
                    return
                }
                let (status, body) = answer(request)
                let chunk = request.method == "PUT"
                if chunk {
                    lock.withLock {
                        chunksInFlight += 1
                        mostChunks = max(mostChunks, chunksInFlight)
                    }
                }
                let respond = {
                    if chunk { self.lock.withLock { self.chunksInFlight -= 1 } }
                    connection.send(content: Self.response(status, body), completion: .contentProcessed { _ in
                        connection.cancel()
                    })
                }
                let hold = lock.withLock {
                    guard request.method == "PUT", holdsFirstChunk else { return false }
                    holdsFirstChunk = false
                    held = respond
                    return true
                }
                /* A Worker answer takes a little while; the delay opens the window two
                 * passes would share. */
                if !hold { DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(30), execute: respond) }
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
                objectOf[uploadId] = plan.objectId
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
        let objectId: String
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

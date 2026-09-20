import Foundation

struct SelfDeviceResponse: Decodable {
    var deviceId: String
    var signingPublicKey: String
    var pairingPublicKey: String
    var status: String
    var envelope: String?
    var protocolVersion: Int?
}

struct UploadCreatedResponse: Decodable {
    var uploadId: String
    var state: String
    var acceptedIndexes: [Int]
}

struct UploadChunkResponse: Decodable {
    var uploadId: String
    var index: Int
    var accepted: Bool
}

struct UploadCommittedResponse: Decodable {
    var uploadId: String
    var state: String
    var revisionId: String?
    var changeSequence: Int64?
}

/// One head or change row; the same shape on `/v1/changes` and `/v1/snapshot`.
struct ChangeRow: Decodable, Equatable {
    var objectId: String
    var revisionId: String
    var sequence: Int64
    var tombstone: Bool
}

struct ChangesPage: Decodable {
    var changes: [ChangeRow]
    var hasMore: Bool
    var nextCursor: String
}

struct SnapshotPage: Decodable {
    var heads: [ChangeRow]
    var hasMore: Bool
    var after: String?
    var highWater: String
}

struct RevisionEnvelope: Decodable {
    var objectId: String
    var revisionId: String
    var chunkCount: Int
    var cryptoVersion: Int
    var manifestSha256: String
    var totalBytes: Int64
    var writerDeviceId: String
}

struct RevisionManifestResponse: Decodable {
    var envelope: RevisionEnvelope
    /// The sealed manifest, base64url.
    var manifest: String
}

struct TombstoneResponse: Decodable {
    var objectId: String
    var revisionId: String
    var tombstone: Bool
}

/// The `DELETE /v1/objects/{id}` body; camelCase like every request body.
struct TombstoneRequest: Encodable {
    var baseRevisionId: String
    var formatVersion: Int
    var reason: String
    var tombstoneRevisionId: String
    var writerSignature: String
}

enum CompanionError: Error, Equatable {
    case invalidEndpoint
    case transport
    case invalidResponse(status: Int)
    /// A Worker `ApiErrorCode` with the HTTP status it arrived on.
    case api(code: String, status: Int)
    case signing

    /// Whether another attempt can plausibly succeed.
    ///
    /// The permanent set is the one where nothing about waiting changes the answer: the
    /// device may not write, the request is wrong, or the Worker already holds different
    /// bytes under this id. `clock_skew` stays retryable because the failing response
    /// itself teaches the client the offset that fixes it.
    var isRetryable: Bool {
        switch self {
        case .transport, .invalidResponse:
            return true
        case .invalidEndpoint, .signing:
            return false
        case let .api(code, _):
            return !CompanionError.permanentCodes.contains(code)
        }
    }

    private static let permanentCodes: Set<String> = [
        "unauthorized", "revoked_device", "invalid_request", "integrity_failed",
        "unsupported_version", "quota_exceeded", "not_found", "idempotency_conflict",
        "chunk_conflict", "stale_revision",
    ]
}

/// The phone's half of the companion protocol: signed requests, learned clock offset,
/// and the three calls an object upload needs.
///
/// An actor because the request timestamp must be monotonic across concurrent uploads,
/// exactly as `client.rs` guarantees with its atomic counter.
actor CompanionClient {
    private let endpoint: URL
    private let session: URLSession
    private var clockOffsetMs: Int64 = 0
    private var lastTimestampMs: UInt64 = 0

    init(endpoint: String) throws {
        guard let url = URL(string: endpoint),
              let scheme = url.scheme,
              scheme == "https" || scheme == "http",
              url.host != nil
        else { throw CompanionError.invalidEndpoint }
        var components = URLComponents()
        components.scheme = scheme
        components.host = url.host
        components.port = url.port
        components.path = "/"
        guard let normalized = components.url else { throw CompanionError.invalidEndpoint }
        self.endpoint = normalized
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        session = URLSession(configuration: configuration)
    }

    /// Learn the Worker's clock from a public route.
    ///
    /// The Worker rejects a signature whose timestamp is more than five minutes from its
    /// own clock, and the desktop rejects a pairing offer that expires outside a
    /// fifteen-minute window. Both are recoverable only by knowing the offset first.
    func syncClock() async throws {
        var request = URLRequest(url: endpoint.appending(path: "healthz"))
        request.httpMethod = "GET"
        _ = try await send(request)
    }

    func nowUtcMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000) + clockOffsetMs
    }

    func selfDevice(
        identity: DeviceIdentity, vaultId: String
    ) async throws -> SelfDeviceResponse {
        try await json(
            identity: identity,
            vaultId: vaultId,
            method: "GET",
            segments: ["v1", "devices", "self"]
        )
    }

    func createObjectUpload(
        identity: DeviceIdentity,
        vaultId: String,
        idempotencyKey: String,
        plan: ObjectUploadPlan
    ) async throws -> UploadCreatedResponse {
        guard let body = try? JSONEncoder().encode(plan) else { throw CompanionError.signing }
        return try await json(
            identity: identity,
            vaultId: vaultId,
            method: "POST",
            segments: ["v1", "uploads"],
            body: body,
            contentType: "application/json",
            idempotencyKey: idempotencyKey
        )
    }

    func putChunk(
        identity: DeviceIdentity,
        vaultId: String,
        idempotencyKey: String,
        uploadId: String,
        index: Int,
        ciphertext: Data
    ) async throws -> UploadChunkResponse {
        var request = try signedRequest(
            identity: identity,
            vaultId: vaultId,
            method: "PUT",
            segments: ["v1", "uploads", uploadId, "chunks", String(index)],
            query: [],
            body: ciphertext,
            contentType: "application/octet-stream",
            idempotencyKey: idempotencyKey
        )
        request.setValue(sha256Base64URL(ciphertext), forHTTPHeaderField: "X-Sona-Chunk-Sha256")
        return try decode(try await send(request))
    }

    func commitUpload(
        identity: DeviceIdentity,
        vaultId: String,
        idempotencyKey: String,
        uploadId: String
    ) async throws -> UploadCommittedResponse {
        let body = Data("{\"version\":1}".utf8)
        return try await json(
            identity: identity,
            vaultId: vaultId,
            method: "POST",
            segments: ["v1", "uploads", uploadId, "commit"],
            body: body,
            contentType: "application/json",
            idempotencyKey: idempotencyKey
        )
    }

    // MARK: - Reading the vault

    /// Every change after `cursor`, oldest first; nil starts from the beginning.
    func changes(
        identity: DeviceIdentity, vaultId: String, cursor: String?
    ) async throws -> ChangesPage {
        var query = [("limit", String(CompanionClient.pageLimit))]
        if let cursor { query.append(("cursor", cursor)) }
        return try await json(
            identity: identity, vaultId: vaultId, method: "GET",
            segments: ["v1", "changes"], query: query
        )
    }

    /// One page of the vault's heads at one high water; the first call passes neither.
    func snapshot(
        identity: DeviceIdentity, vaultId: String, highWater: String?, after: String?
    ) async throws -> SnapshotPage {
        var query = [("limit", String(CompanionClient.pageLimit))]
        if let highWater { query.append(("highWater", highWater)) }
        if let after { query.append(("after", after)) }
        return try await json(
            identity: identity, vaultId: vaultId, method: "GET",
            segments: ["v1", "snapshot"], query: query
        )
    }

    func manifest(
        identity: DeviceIdentity, vaultId: String, objectId: String, revisionId: String
    ) async throws -> RevisionManifestResponse {
        try await json(
            identity: identity, vaultId: vaultId, method: "GET",
            segments: ["v1", "objects", objectId, "revisions", revisionId, "manifest"]
        )
    }

    /// One sealed chunk, exactly as its writer uploaded it.
    func chunk(
        identity: DeviceIdentity, vaultId: String, objectId: String, revisionId: String,
        index: Int
    ) async throws -> Data {
        try await send(try signedRequest(
            identity: identity, vaultId: vaultId, method: "GET",
            segments: ["v1", "objects", objectId, "revisions", revisionId, "chunks", String(index)],
            query: [], body: Data(), contentType: nil, idempotencyKey: nil
        ))
    }

    /// Tombstone an object over its current head; the Worker refuses a stale base.
    func tombstone(
        identity: DeviceIdentity,
        vaultId: String,
        idempotencyKey: String,
        objectId: String,
        baseRevisionId: String,
        tombstoneRevisionId: String
    ) async throws -> TombstoneResponse {
        let reason = "user_request"
        guard let signature = try? signEd25519(
            signingSeed: identity.signingSeed,
            message: canonicalTombstoneBytes(
                CanonicalTombstoneInput(
                    vaultId: vaultId,
                    objectId: objectId,
                    tombstoneRevisionId: tombstoneRevisionId,
                    baseRevisionId: baseRevisionId,
                    reason: reason,
                    formatVersion: UInt64(SonaProtocol.protocolVersion)
                )
            )
        ) else { throw CompanionError.signing }
        let request = TombstoneRequest(
            baseRevisionId: baseRevisionId,
            formatVersion: SonaProtocol.protocolVersion,
            reason: reason,
            tombstoneRevisionId: tombstoneRevisionId,
            writerSignature: Base64URL.encode(signature)
        )
        guard let body = try? JSONEncoder().encode(request) else { throw CompanionError.signing }
        return try await json(
            identity: identity,
            vaultId: vaultId,
            method: "DELETE",
            segments: ["v1", "objects", objectId],
            body: body,
            contentType: "application/json",
            idempotencyKey: idempotencyKey
        )
    }

    private static let pageLimit = 100

    // MARK: - Internals

    private func json<Value: Decodable>(
        identity: DeviceIdentity,
        vaultId: String,
        method: String,
        segments: [String],
        query: [(String, String)] = [],
        body: Data = Data(),
        contentType: String? = nil,
        idempotencyKey: String? = nil
    ) async throws -> Value {
        let request = try signedRequest(
            identity: identity,
            vaultId: vaultId,
            method: method,
            segments: segments,
            query: query,
            body: body,
            contentType: contentType,
            idempotencyKey: idempotencyKey
        )
        return try decode(try await send(request))
    }

    private func signedRequest(
        identity: DeviceIdentity,
        vaultId: String,
        method: String,
        segments: [String],
        query: [(String, String)],
        body: Data,
        contentType: String?,
        idempotencyKey: String?
    ) throws -> URLRequest {
        var url = endpoint
        for segment in segments {
            url = url.appending(path: segment)
        }
        if !query.isEmpty {
            url.append(queryItems: query.map { URLQueryItem(name: $0.0, value: $0.1) })
        }
        let nonce = randomBytes(16)
        let timestamp = nextTimestampMs()
        let input = CanonicalRequestInput(
            vaultId: vaultId,
            deviceId: identity.deviceId,
            method: method,
            path: url.path(),
            query: query,
            bodyDigest: sha256Base64URL(body),
            contentType: contentType ?? "",
            idempotencyKey: idempotencyKey ?? "",
            timestamp: timestamp,
            nonce: nonce
        )
        guard let signature = try? signEd25519(
            signingSeed: identity.signingSeed, message: canonicalRequestBytes(input)
        ) else { throw CompanionError.signing }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue(vaultId, forHTTPHeaderField: "X-Sona-Vault-Id")
        request.setValue(identity.deviceId, forHTTPHeaderField: "X-Sona-Device-Id")
        request.setValue(String(timestamp), forHTTPHeaderField: "X-Sona-Timestamp")
        request.setValue(Base64URL.encode(nonce), forHTTPHeaderField: "X-Sona-Nonce")
        request.setValue(Base64URL.encode(signature), forHTTPHeaderField: "X-Sona-Signature")
        if let idempotencyKey {
            request.setValue(idempotencyKey, forHTTPHeaderField: "X-Sona-Idempotency-Key")
        }
        if let contentType {
            request.setValue(contentType, forHTTPHeaderField: "Content-Type")
            request.setValue(String(body.count), forHTTPHeaderField: "Content-Length")
            request.httpBody = body
        }
        return request
    }

    /// The Worker requires a 13-digit millisecond timestamp; the counter keeps two
    /// requests started in the same millisecond from sharing one.
    private func nextTimestampMs() -> UInt64 {
        let observed = UInt64(max(0, nowUtcMs()))
        lastTimestampMs = max(observed, lastTimestampMs + 1)
        return lastTimestampMs
    }

    private func send(_ request: URLRequest) async throws -> Data {
        let bytes: Data
        let response: HTTPURLResponse
        do {
            let (received, urlResponse) = try await session.data(for: request)
            guard let httpResponse = urlResponse as? HTTPURLResponse else {
                throw CompanionError.invalidResponse(status: 0)
            }
            bytes = received
            response = httpResponse
        } catch let error as CompanionError {
            throw error
        } catch {
            throw CompanionError.transport
        }
        observeServerDate(response)
        guard (200..<300).contains(response.statusCode) else {
            throw apiError(bytes, status: response.statusCode)
        }
        return bytes
    }

    private func observeServerDate(_ response: HTTPURLResponse) {
        guard let text = response.value(forHTTPHeaderField: "Date"),
              let serverDate = CompanionClient.httpDateFormatter.date(from: text)
        else { return }
        let serverMs = Int64(serverDate.timeIntervalSince1970 * 1000)
        let localMs = Int64(Date().timeIntervalSince1970 * 1000)
        clockOffsetMs = serverMs - localMs
    }

    /// The Worker's error body is flat: `{ code, request_id, retryable }`.
    private func apiError(_ bytes: Data, status: Int) -> CompanionError {
        struct WireError: Decodable {
            var code: String
        }
        guard let wire = try? JSONDecoder().decode(WireError.self, from: bytes) else {
            return .invalidResponse(status: status)
        }
        return .api(code: wire.code, status: status)
    }

    private func decode<Value: Decodable>(_ bytes: Data) throws -> Value {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let value = try? decoder.decode(Value.self, from: bytes) else {
            throw CompanionError.invalidResponse(status: 200)
        }
        return value
    }

    private static let httpDateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "GMT")
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss 'GMT'"
        return formatter
    }()
}

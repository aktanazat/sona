import Foundation

/// A meeting recording's part of a queued object.
struct QueuedRecording: Codable, Equatable {
    var durationMs: Int64
    var title: String
    var audioByteLength: Int
    var audioSha256: String
}

/// A thought's part of a queued object. Attachment files live in the item directory
/// under the names the attachments carry.
struct QueuedThought: Codable, Equatable {
    var origin: ThoughtOrigin
    var text: String
    var link: String?
    var attachments: [ThoughtAttachment]
}

enum QueuedPayload: Codable, Equatable {
    case recording(QueuedRecording)
    case thought(QueuedThought)
}

/// One object on its way into the vault.
///
/// Every id is minted when the object is enqueued and never regenerated, so a retry is
/// the same write to the Worker rather than a second object.
struct QueuedObject: Codable, Equatable, Identifiable {
    var objectId: String
    var revisionId: String
    var uploadId: String
    var capturedAtUtcMs: Int64
    var payload: QueuedPayload
    /// The vault whose root the staged ciphertext is bound to, once staged.
    var stagedForVaultId: String?
    var chunkSizes: [Int]
    var chunkDigests: [String]
    var attempts: Int
    var nextAttemptUtcMs: Int64
    var lastError: String?
    /// Set when the Worker refused in a way no retry can change.
    var parked: Bool

    var id: String { objectId }
}

enum UploadQueueError: Error, Equatable {
    case notPaired
    case missingAudio
    case commitIncomplete(state: String)
}

/// The on-disk outbox. Capture writes into it; uploading drains it; nothing else owns
/// a queued object's lifetime.
actor UploadQueue {
    private static let recordingAudioFile = "audio.pcm"

    private let root: URL
    private let identity: DeviceIdentity
    private var credentials: VaultCredentials?
    private var client: CompanionClient?

    init(root: URL, identity: DeviceIdentity, credentials: VaultCredentials?) {
        self.root = root
        self.identity = identity
        self.credentials = credentials
        try? FileManager.default.createDirectory(
            at: root, withIntermediateDirectories: true
        )
    }

    func setCredentials(_ value: VaultCredentials?) {
        credentials = value
        client = nil
    }

    func items() -> [QueuedObject] {
        itemDirectories().compactMap(loadItem).sorted { $0.capturedAtUtcMs < $1.capturedAtUtcMs }
    }

    /// Take ownership of a finished capture. The audio file is moved, not copied, so
    /// there is one copy of the bytes from here on.
    func enqueue(
        audio: CapturedAudio,
        recordedAtUtcMs: Int64,
        title: String
    ) throws -> QueuedObject {
        let objectId = randomOpaqueId()
        try adopt(audio.url, as: UploadQueue.recordingAudioFile, objectId: objectId)
        let item = newItem(
            objectId: objectId,
            capturedAtUtcMs: recordedAtUtcMs,
            payload: .recording(
                QueuedRecording(
                    durationMs: audio.durationMs,
                    title: title,
                    audioByteLength: audio.byteLength,
                    audioSha256: audio.sha256
                )
            )
        )
        try save(item)
        return item
    }

    /// Take ownership of a captured thought and every file behind its attachments.
    func enqueue(
        thought: QueuedThought,
        capturedAtUtcMs: Int64,
        files: [URL]
    ) throws -> QueuedObject {
        precondition(files.count == thought.attachments.count)
        let objectId = randomOpaqueId()
        for (file, attachment) in zip(files, thought.attachments) {
            try adopt(file, as: attachment.file, objectId: objectId)
        }
        let item = newItem(
            objectId: objectId, capturedAtUtcMs: capturedAtUtcMs, payload: .thought(thought)
        )
        try save(item)
        return item
    }

    /// Try every item that is due, oldest first, one at a time.
    ///
    /// A retryable failure ends the pass: the network or the Worker is unhappy and the
    /// remaining items would fail the same way.
    func drain() async {
        guard let credentials else { return }
        let client: CompanionClient
        do {
            client = try self.client ?? CompanionClient(endpoint: credentials.endpoint)
        } catch {
            return
        }
        self.client = client
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        for item in items() where !item.parked && item.nextAttemptUtcMs <= now {
            do {
                try await upload(item, credentials: credentials, client: client)
            } catch {
                let retryable = (error as? CompanionError)?.isRetryable ?? true
                record(failure: error, on: item, retryable: retryable)
                if retryable { return }
            }
        }
    }

    /// Clear every refusal so the next drain tries again.
    ///
    /// A refusal is only ever final for the reason that produced it; the operator
    /// reopening the app is the signal that the reason may have changed.
    func retryParked() {
        for var item in items() where item.parked {
            item.parked = false
            item.attempts = 0
            item.nextAttemptUtcMs = 0
            try? save(item)
        }
    }

    func discard(objectId: String) {
        try? FileManager.default.removeItem(at: root.appending(path: directoryName(objectId)))
    }

    // MARK: - Upload

    private func upload(
        _ item: QueuedObject,
        credentials: VaultCredentials,
        client: CompanionClient
    ) async throws {
        let item = try stage(item, credentials: credentials)
        let plan = try plan(item, credentials: credentials)
        let scope = idempotencyScope(item)
        let created = try await client.createObjectUpload(
            identity: identity,
            vaultId: credentials.vaultId,
            idempotencyKey: stableIdempotencyKey([scope, item.objectId, item.revisionId, "create"]),
            plan: plan
        )
        let accepted = Set(created.acceptedIndexes)
        for index in item.chunkSizes.indices where !accepted.contains(index) {
            let ciphertext = try Data(contentsOf: chunkURL(item, index: index))
            _ = try await client.putChunk(
                identity: identity,
                vaultId: credentials.vaultId,
                idempotencyKey: stableIdempotencyKey([
                    scope, item.objectId, item.revisionId, "chunk-\(index)",
                ]),
                uploadId: item.uploadId,
                index: index,
                ciphertext: ciphertext
            )
        }
        let committed = try await client.commitUpload(
            identity: identity,
            vaultId: credentials.vaultId,
            idempotencyKey: stableIdempotencyKey([scope, item.objectId, item.revisionId, "commit"]),
            uploadId: item.uploadId
        )
        guard committed.state == "committed" else {
            throw UploadQueueError.commitIncomplete(state: committed.state)
        }
        discard(objectId: item.objectId)
    }

    /// Encrypt the manifest and every chunk once, to disk.
    ///
    /// Nonces are random, so ciphertext cannot be reproduced: staging it makes a retry
    /// byte-identical, which is what the Worker's chunk digests require.
    private func stage(
        _ item: QueuedObject, credentials: VaultCredentials
    ) throws -> QueuedObject {
        var item = item
        if item.stagedForVaultId == credentials.vaultId, !item.chunkDigests.isEmpty {
            return item
        }
        if item.stagedForVaultId != nil {
            /* A re-pairing moved the vault under this object; its AAD names the old one,
             * so the staged bytes are unusable and the revision starts over. */
            try? removeStagedFiles(item)
            item.revisionId = randomOpaqueId()
            item.uploadId = randomOpaqueId()
        }
        let chunkCount = self.chunkCount(item)
        let sealedManifest = try sealObjectRevisionPayload(
            vaultRoot: credentials.vaultRoot,
            context: context(
                item, credentials: credentials,
                index: 0, total: chunkCount, contentKind: .manifest
            ),
            nonce: randomBytes(12),
            plaintext: try manifestPlaintext(item)
        )
        try sealedManifest.write(to: manifestURL(item), options: .atomic)
        var sizes: [Int] = []
        var digests: [String] = []
        for index in 0..<chunkCount {
            let sealed = try sealObjectRevisionPayload(
                vaultRoot: credentials.vaultRoot,
                context: context(
                    item, credentials: credentials,
                    index: index, total: chunkCount, contentKind: .chunk
                ),
                nonce: randomBytes(12),
                plaintext: try chunkPlaintext(item, index: index)
            )
            try sealed.write(to: chunkURL(item, index: index), options: .atomic)
            sizes.append(sealed.count)
            digests.append(sha256Base64URL(sealed))
        }
        item.stagedForVaultId = credentials.vaultId
        item.chunkSizes = sizes
        item.chunkDigests = digests
        try save(item)
        return item
    }

    private func plan(
        _ item: QueuedObject, credentials: VaultCredentials
    ) throws -> ObjectUploadPlan {
        let manifest = try Data(contentsOf: manifestURL(item))
        let manifestDigest = sha256Base64URL(manifest)
        let chunks = item.chunkSizes.indices.map { index in
            ObjectUploadPlan.Chunk(
                index: index,
                size: item.chunkSizes[index],
                sha256: item.chunkDigests[index]
            )
        }
        let totalBytes = item.chunkSizes.reduce(0, +)
        let signature = try signEd25519(
            signingSeed: identity.signingSeed,
            message: canonicalUploadEnvelopeBytes(
                CanonicalUploadEnvelopeInput(
                    vaultId: credentials.vaultId,
                    kind: "object",
                    objectId: item.objectId,
                    revisionId: item.revisionId,
                    baseRevisionId: nil,
                    shareId: nil,
                    manifestDigest: manifestDigest,
                    cryptoVersion: UInt64(SonaProtocol.cryptoVersion),
                    totalBytes: UInt64(totalBytes),
                    chunks: chunks.map {
                        UploadEnvelopeChunk(
                            index: UInt64($0.index), size: UInt64($0.size), sha256: $0.sha256
                        )
                    }
                )
            )
        )
        return ObjectUploadPlan(
            version: SonaProtocol.protocolVersion,
            cryptoVersion: SonaProtocol.cryptoVersion,
            uploadId: item.uploadId,
            objectId: item.objectId,
            revisionId: item.revisionId,
            manifest: Base64URL.encode(manifest),
            manifestSha256: manifestDigest,
            chunks: chunks,
            chunkCount: chunks.count,
            totalBytes: totalBytes,
            writerSignature: Base64URL.encode(signature)
        )
    }

    // MARK: - What each payload seals

    private func context(
        _ item: QueuedObject,
        credentials: VaultCredentials,
        index: Int,
        total: Int,
        contentKind: ObjectContentKind
    ) -> ObjectRevisionCryptoContext {
        ObjectRevisionCryptoContext(
            vaultId: credentials.vaultId,
            objectId: item.objectId,
            revisionId: item.revisionId,
            index: UInt64(index),
            total: UInt64(total),
            contentKind: contentKind,
            sourceFormat: sourceFormat(item)
        )
    }

    private func sourceFormat(_ item: QueuedObject) -> String {
        switch item.payload {
        case .recording: return DeviceRecordingObject.sourceFormat
        case .thought: return ThoughtObject.sourceFormat
        }
    }

    /// The first idempotency-key part, which keeps a recording's keys byte-identical
    /// to the ones the app derived before thoughts existed.
    private func idempotencyScope(_ item: QueuedObject) -> String {
        switch item.payload {
        case .recording: return "device-recording"
        case .thought: return "thought"
        }
    }

    private func chunkCount(_ item: QueuedObject) -> Int {
        switch item.payload {
        case let .recording(recording):
            return DeviceRecordingObject.chunkCount(audioByteLength: recording.audioByteLength)
        case let .thought(thought):
            return ThoughtObject.chunkCount(thought.attachments)
        }
    }

    private func manifestPlaintext(_ item: QueuedObject) throws -> Data {
        switch item.payload {
        case let .recording(recording):
            return try DeviceRecordingObject.encodeManifest(
                DeviceRecordingObject.manifest(
                    deviceId: identity.deviceId,
                    recordedAtUtcMs: item.capturedAtUtcMs,
                    durationMs: recording.durationMs,
                    title: recording.title,
                    audioByteLength: recording.audioByteLength,
                    audioSha256: recording.audioSha256
                )
            )
        case let .thought(thought):
            return try ThoughtObject.encodeManifest(
                ThoughtObject.manifest(
                    deviceId: identity.deviceId,
                    capturedAtUtcMs: item.capturedAtUtcMs,
                    origin: thought.origin,
                    text: thought.text,
                    link: thought.link.flatMap(URL.init(string:)),
                    attachments: thought.attachments
                )
            )
        }
    }

    private func chunkPlaintext(_ item: QueuedObject, index: Int) throws -> Data {
        switch item.payload {
        case let .recording(recording):
            let range = DeviceRecordingObject.chunkRange(
                index: index, audioByteLength: recording.audioByteLength
            )
            return try readSlice(itemFile(item, UploadQueue.recordingAudioFile), range)
        case let .thought(thought):
            guard let (attachment, range) = ThoughtObject.slice(thought.attachments, index: index)
            else { return Data() }
            return try readSlice(itemFile(item, attachment.file), range)
        }
    }

    private func readSlice(_ url: URL, _ range: Range<Int>) throws -> Data {
        guard let handle = try? FileHandle(forReadingFrom: url) else {
            throw UploadQueueError.missingAudio
        }
        defer { try? handle.close() }
        try handle.seek(toOffset: UInt64(range.lowerBound))
        return try handle.read(upToCount: range.count) ?? Data()
    }

    // MARK: - Persistence

    private func newItem(
        objectId: String, capturedAtUtcMs: Int64, payload: QueuedPayload
    ) -> QueuedObject {
        QueuedObject(
            objectId: objectId,
            revisionId: randomOpaqueId(),
            uploadId: randomOpaqueId(),
            capturedAtUtcMs: capturedAtUtcMs,
            payload: payload,
            stagedForVaultId: nil,
            chunkSizes: [],
            chunkDigests: [],
            attempts: 0,
            nextAttemptUtcMs: 0,
            lastError: nil,
            parked: false
        )
    }

    /// Move a captured file into the item directory under its outbox name.
    private func adopt(_ source: URL, as name: String, objectId: String) throws {
        let directory = root.appending(path: directoryName(objectId))
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let destination = directory.appending(path: name)
        if FileManager.default.fileExists(atPath: destination.path) {
            try FileManager.default.removeItem(at: destination)
        }
        try FileManager.default.moveItem(at: source, to: destination)
    }

    private func record(failure: Error, on item: QueuedObject, retryable: Bool) {
        guard var stored = loadItem(root.appending(path: directoryName(item.objectId))) else {
            return
        }
        stored.attempts += 1
        stored.lastError = String(describing: failure)
        stored.parked = !retryable
        /* Bounded backoff: a flapping network must not turn the outbox into a spin. */
        let delaySeconds = min(60 * (1 << min(stored.attempts - 1, 4)), 900)
        stored.nextAttemptUtcMs =
            Int64(Date().timeIntervalSince1970 * 1000) + Int64(delaySeconds) * 1000
        try? save(stored)
    }

    private func itemDirectories() -> [URL] {
        (try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: nil
        )) ?? []
    }

    private func loadItem(_ directory: URL) -> QueuedObject? {
        guard let bytes = try? Data(contentsOf: directory.appending(path: "item.json")) else {
            return nil
        }
        let decoder = JSONDecoder()
        if let item = try? decoder.decode(QueuedObject.self, from: bytes) {
            return item
        }
        guard let legacy = try? decoder.decode(LegacyQueuedRecording.self, from: bytes) else {
            return nil
        }
        let item = legacy.object
        try? save(item)
        return item
    }

    private func save(_ item: QueuedObject) throws {
        let directory = root.appending(path: directoryName(item.objectId))
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try JSONEncoder().encode(item).write(
            to: directory.appending(path: "item.json"), options: .atomic
        )
    }

    private func removeStagedFiles(_ item: QueuedObject) throws {
        let directory = root.appending(path: directoryName(item.objectId))
        for name in try FileManager.default.contentsOfDirectory(atPath: directory.path)
        where name.hasPrefix("chunk-") || name == "manifest.bin" {
            try FileManager.default.removeItem(at: directory.appending(path: name))
        }
    }

    /// Object ids are base64url, which is already a safe directory name.
    private func directoryName(_ objectId: String) -> String { objectId }

    private func itemFile(_ item: QueuedObject, _ name: String) -> URL {
        root.appending(path: directoryName(item.objectId)).appending(path: name)
    }

    private func manifestURL(_ item: QueuedObject) -> URL {
        itemFile(item, "manifest.bin")
    }

    private func chunkURL(_ item: QueuedObject, index: Int) -> URL {
        itemFile(item, String(format: "chunk-%06d.bin", index))
    }
}

/// The item shape the app wrote before thoughts existed. A phone that updates with
/// recordings still in its outbox keeps them: the ids, the staged ciphertext, and the
/// idempotency keys all carry over unchanged.
private struct LegacyQueuedRecording: Decodable {
    var objectId: String
    var revisionId: String
    var uploadId: String
    var recordedAtUtcMs: Int64
    var durationMs: Int64
    var title: String
    var audioByteLength: Int
    var audioSha256: String
    var stagedForVaultId: String?
    var chunkSizes: [Int]
    var chunkDigests: [String]
    var attempts: Int
    var nextAttemptUtcMs: Int64
    var lastError: String?
    var parked: Bool

    var object: QueuedObject {
        QueuedObject(
            objectId: objectId,
            revisionId: revisionId,
            uploadId: uploadId,
            capturedAtUtcMs: recordedAtUtcMs,
            payload: .recording(
                QueuedRecording(
                    durationMs: durationMs,
                    title: title,
                    audioByteLength: audioByteLength,
                    audioSha256: audioSha256
                )
            ),
            stagedForVaultId: stagedForVaultId,
            chunkSizes: chunkSizes,
            chunkDigests: chunkDigests,
            attempts: attempts,
            nextAttemptUtcMs: nextAttemptUtcMs,
            lastError: lastError,
            parked: parked
        )
    }
}

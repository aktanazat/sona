import Foundation

/// The phone's real `UploadQueue`, built for macOS from `mobile/Shared`, against a local
/// copy of the real companion Worker (app-audit SONA-C3 and SONA-C4).
///
///   check mint <identity.json>
///   check run <identity.json> <vault-id> <proxy-url> <purge-command>
///
/// `run` exits 0 only when both checks pass:
///   C3: two outbox passes started together upload every item once; nothing is refused.
///   C4: an upload the Worker purged after its create starts over and commits.
@main
enum OutboxWorkerCheck {
    static func main() async throws {
        let arguments = CommandLine.arguments.dropFirst()
        switch arguments.first {
        case "mint":
            guard let path = arguments.dropFirst().first else { throw Usage() }
            try JSONEncoder().encode(DeviceIdentity.mint()).write(to: URL(filePath: path))
        case "run":
            let rest = Array(arguments.dropFirst())
            guard rest.count == 4 else { throw Usage() }
            let identity = try JSONDecoder().decode(DeviceIdentity.self, from: Data(contentsOf: URL(filePath: rest[0])))
            let credentials = VaultCredentials(
                endpoint: rest[2], vaultId: rest[1], vaultRootBase64URL: Base64URL.encode(randomBytes(32))
            )
            let c3 = try await twoPassesSendEachItemOnce(identity, credentials)
            let c4 = try await purgedUploadStartsOver(identity, credentials, proxy: rest[2], purge: rest[3])
            print("C3 \(c3 ? "pass" : "FAIL")")
            print("C4 \(c4 ? "pass" : "FAIL")")
            exit(c3 && c4 ? 0 : 1)
        default:
            throw Usage()
        }
    }

    private static func twoPassesSendEachItemOnce(
        _ identity: DeviceIdentity, _ credentials: VaultCredentials
    ) async throws -> Bool {
        let queue = UploadQueue(root: scratch("c3"), identity: identity, credentials: credentials)
        for index in 0..<4 {
            _ = try await queue.enqueue(
                thought: QueuedThought(origin: .typed, text: "Thought \(index)", link: nil, attachments: []),
                capturedAtUtcMs: Int64(index + 1), files: []
            )
        }
        async let first: Void = queue.drain()
        async let second: Void = queue.drain()
        _ = await (first, second)
        let left = await queue.items()
        for item in left { print("C3 left: \(item.objectId) parked=\(item.parked) error=\(item.lastError ?? "")") }
        return left.isEmpty
    }

    private static func purgedUploadStartsOver(
        _ identity: DeviceIdentity, _ credentials: VaultCredentials, proxy: String, purge: String
    ) async throws -> Bool {
        let root = scratch("c4")
        let queue = UploadQueue(root: root, identity: identity, credentials: credentials)
        let item = try await queue.enqueue(
            thought: QueuedThought(origin: .typed, text: "Purged upload", link: nil, attachments: []),
            capturedAtUtcMs: 1, files: []
        )
        _ = try await URLSession.shared.data(for: post(proxy, "/__check/drop-next-chunk"))
        await queue.drain()
        let (uploadBytes, _) = try await URLSession.shared.data(from: URL(string: proxy + "/__check/last-upload")!)
        let uploadId = String(decoding: uploadBytes, as: UTF8.self)
        print("C4 created \(uploadId), chunk dropped; purging it")
        let process = Process()
        process.executableURL = URL(filePath: "/bin/sh")
        process.arguments = ["-c", purge, "purge", uploadId]
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { return false }

        /* A day passes: the retry the dropped chunk scheduled is due. */
        let itemFile = root.appending(path: item.objectId).appending(path: "item.json")
        var stored = try JSONDecoder().decode(QueuedObject.self, from: Data(contentsOf: itemFile))
        stored.nextAttemptUtcMs = 0
        try JSONEncoder().encode(stored).write(to: itemFile)

        for _ in 0..<2 { await queue.drain() }
        let left = await queue.items()
        for item in left { print("C4 left: \(item.objectId) parked=\(item.parked) error=\(item.lastError ?? "")") }
        return left.isEmpty
    }

    private static func scratch(_ name: String) -> URL {
        FileManager.default.temporaryDirectory.appending(path: "outbox-worker-check-\(name)-\(UUID().uuidString)")
    }

    private static func post(_ base: String, _ path: String) -> URLRequest {
        var request = URLRequest(url: URL(string: base + path)!)
        request.httpMethod = "POST"
        return request
    }

    private struct Usage: Error {}
}

import XCTest

final class UploadQueueTests: XCTestCase {
    /// A recording queued by the app before thoughts existed is still the same object
    /// after the update: same ids, same staged chunks, so the retry it was waiting on is
    /// the same write to the Worker.
    func testItemWrittenByThePreviousAppVersionIsStillListed() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(path: "outbox-\(UUID().uuidString)")
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        let directory = root.appending(path: "obj_legacy_0001")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data(
            """
            {"objectId":"obj_legacy_0001","revisionId":"rev_legacy_0001",\
            "uploadId":"upl_legacy_0001","recordedAtUtcMs":1700000000000,\
            "durationMs":1500,"title":"Phone recording","audioByteLength":48000,\
            "audioSha256":"T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8",\
            "stagedForVaultId":"vault_0001","chunkSizes":[48028],\
            "chunkDigests":["digest_0001"],"attempts":2,"nextAttemptUtcMs":0,\
            "lastError":"offline","parked":false}
            """.utf8
        ).write(to: directory.appending(path: "item.json"))

        let queue = UploadQueue(root: root, identity: .mint(), credentials: nil)
        let expected = QueuedObject(
            objectId: "obj_legacy_0001",
            revisionId: "rev_legacy_0001",
            uploadId: "upl_legacy_0001",
            capturedAtUtcMs: 1_700_000_000_000,
            payload: .recording(
                QueuedRecording(
                    durationMs: 1500,
                    title: "Phone recording",
                    audioByteLength: 48000,
                    audioSha256: "T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8"
                )
            ),
            stagedForVaultId: "vault_0001",
            chunkSizes: [48028],
            chunkDigests: ["digest_0001"],
            attempts: 2,
            nextAttemptUtcMs: 0,
            lastError: "offline",
            parked: false
        )
        let items = await queue.items()
        XCTAssertEqual(items, [expected])
    }
}

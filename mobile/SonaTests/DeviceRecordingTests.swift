import XCTest

/// The shape of a `device_recording` object is a cross-process contract: the desktop
/// reader Main adds has to find exactly these bytes and this chunking.
final class DeviceRecordingTests: XCTestCase {
    func testManifestIsTheAgreedJson() throws {
        let manifest = DeviceRecordingObject.manifest(
            deviceId: "phone_device_0001",
            recordedAtUtcMs: 1_700_000_000_000,
            durationMs: 1500,
            title: "Phone recording",
            audioByteLength: 48000,
            audioSha256: "T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8"
        )
        let encoded = try DeviceRecordingObject.encodeManifest(manifest)
        XCTAssertEqual(
            try JSONSerialization.data(
                withJSONObject: JSONSerialization.jsonObject(with: encoded),
                options: .sortedKeys
            ),
            Data("""
            {"audio":{"byte_length":48000,"channels":1,"codec":"pcm_s16le",\
            "sample_rate_hz":16000,"sha256":"T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8"},\
            "device_id":"phone_device_0001","duration_ms":1500,"format_version":1,\
            "kind":"device_recording","recorded_at_utc_ms":1700000000000,\
            "title":"Phone recording"}
            """.utf8)
        )
        XCTAssertEqual(
            try JSONDecoder().decode(DeviceRecordingManifest.self, from: encoded), manifest
        )
    }

    /// Every chunk but the last must encrypt to exactly the Worker's 4 MiB ceiling, or
    /// `parseChunks` rejects the plan.
    func testChunkingFillsEveryChunkButTheLast() {
        let plaintextCeiling = DeviceRecordingObject.maxPlaintextChunkBytes
        XCTAssertEqual(DeviceRecordingObject.maxEncryptedChunkBytes, 4 * 1024 * 1024)
        XCTAssertEqual(plaintextCeiling, 4 * 1024 * 1024 - 28)

        XCTAssertEqual(DeviceRecordingObject.chunkCount(audioByteLength: 0), 1)
        XCTAssertEqual(DeviceRecordingObject.chunkCount(audioByteLength: 1), 1)
        XCTAssertEqual(
            DeviceRecordingObject.chunkCount(audioByteLength: plaintextCeiling), 1
        )
        XCTAssertEqual(
            DeviceRecordingObject.chunkCount(audioByteLength: plaintextCeiling + 1), 2
        )

        let length = plaintextCeiling * 2 + 7
        let count = DeviceRecordingObject.chunkCount(audioByteLength: length)
        XCTAssertEqual(count, 3)
        var covered = 0
        for index in 0..<count {
            let range = DeviceRecordingObject.chunkRange(index: index, audioByteLength: length)
            XCTAssertEqual(range.lowerBound, covered)
            covered = range.upperBound
            if index < count - 1 {
                XCTAssertEqual(range.count + 28, DeviceRecordingObject.maxEncryptedChunkBytes)
            }
        }
        XCTAssertEqual(covered, length)
    }

    func testUploadPlanUsesCamelCaseKeys() throws {
        let plan = ObjectUploadPlan(
            version: 1,
            cryptoVersion: 1,
            uploadId: "upload_0000000001",
            objectId: "object_0000000001",
            revisionId: "revision_000000001",
            manifest: "AAAA",
            manifestSha256: "T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8",
            chunks: [ObjectUploadPlan.Chunk(index: 0, size: 60, sha256: "digest")],
            chunkCount: 1,
            totalBytes: 60,
            writerSignature: "signature"
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let keys = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try encoder.encode(plan)) as? [String: Any]
        ).keys.sorted()
        XCTAssertEqual(
            keys,
            [
                "chunkCount", "chunks", "cryptoVersion", "manifest", "manifestSha256",
                "objectId", "revisionId", "totalBytes", "uploadId", "version",
                "writerSignature",
            ],
            "the Worker's assertExactKeys allows exactly these plus baseRevisionId"
        )
    }

    func testStableIdempotencyKeyIsAnOpaqueIdAndDependsOnEveryPart() {
        let key = stableIdempotencyKey(["device-recording", "object", "revision", "commit"])
        XCTAssertEqual(key.count, 43)
        XCTAssertNotNil(Base64URL.decode(key))
        XCTAssertNotEqual(
            key, stableIdempotencyKey(["device-recording", "object", "revision", "create"])
        )
        /* The null separator is what keeps neighbouring parts from merging. */
        XCTAssertNotEqual(stableIdempotencyKey(["ab", "c"]), stableIdempotencyKey(["a", "bc"]))
    }
}

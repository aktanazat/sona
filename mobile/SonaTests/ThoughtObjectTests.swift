import XCTest

/// A thought's manifest is a cross-process contract: the sorter parses exactly these
/// keys, with `link` and `audio` present as null rather than absent.
final class ThoughtObjectTests: XCTestCase {
    func testTextOnlyManifestCarriesEveryKeyWithNullsAndOneEmptyChunk() throws {
        let manifest = ThoughtObject.manifest(
            deviceId: "phone_device_0001",
            capturedAtUtcMs: 1_700_000_000_000,
            origin: .typed,
            text: "buy the blue one",
            link: nil,
            attachments: []
        )
        let encoded = try ThoughtObject.encodeManifest(manifest)
        XCTAssertEqual(
            String(decoding: encoded, as: UTF8.self),
            """
            {"audio":null,"captured_at_utc_ms":1700000000000,"device_id":"phone_device_0001",\
            "format_version":1,"images":[],"kind":"thought","link":null,"origin":"typed",\
            "text":"buy the blue one"}
            """
        )
        XCTAssertEqual(try JSONDecoder().decode(ThoughtManifest.self, from: encoded), manifest)
        XCTAssertEqual(ThoughtObject.chunkCount([]), 1)
        XCTAssertNil(ThoughtObject.slice([], index: 0))
    }

    /// Attachments take consecutive chunk ranges in order: the audio first, then each
    /// image, an image larger than one chunk spanning as many as it needs.
    func testAttachmentsMapToConsecutiveChunkRangesInOrder() {
        let ceiling = DeviceRecordingObject.maxPlaintextChunkBytes
        let attachments = [
            ThoughtAttachment(
                kind: .audio(durationMs: 1500), file: "audio.pcm", byteLength: 48000,
                sha256: "audio-sha"
            ),
            ThoughtAttachment(
                kind: .image(mime: "image/jpeg", width: 2048, height: 1536), file: "image-0.jpg",
                byteLength: ceiling + 1, sha256: "image-sha"
            ),
            ThoughtAttachment(
                kind: .image(mime: "image/jpeg", width: 100, height: 100), file: "image-1.jpg",
                byteLength: 10, sha256: "small-sha"
            ),
        ]
        let manifest = ThoughtObject.manifest(
            deviceId: "phone_device_0001",
            capturedAtUtcMs: 1,
            origin: .voice,
            text: "",
            link: URL(string: "https://example.com/a?b=c"),
            attachments: attachments
        )
        XCTAssertEqual(ThoughtObject.chunkCount(attachments), 4)
        XCTAssertEqual(manifest.audio?.chunk_start, 0)
        XCTAssertEqual(manifest.audio?.chunk_count, 1)
        XCTAssertEqual(manifest.audio?.duration_ms, 1500)
        XCTAssertEqual(manifest.images.map(\.chunk_start), [1, 3])
        XCTAssertEqual(manifest.images.map(\.chunk_count), [2, 1])
        XCTAssertEqual(manifest.link?.url, "https://example.com/a?b=c")

        let second = ThoughtObject.slice(attachments, index: 2)
        XCTAssertEqual(second?.attachment.file, "image-0.jpg")
        XCTAssertEqual(second?.range, ceiling..<(ceiling + 1))
        let third = ThoughtObject.slice(attachments, index: 3)
        XCTAssertEqual(third?.attachment.file, "image-1.jpg")
        XCTAssertEqual(third?.range, 0..<10)
        XCTAssertNil(ThoughtObject.slice(attachments, index: 4))
    }
}

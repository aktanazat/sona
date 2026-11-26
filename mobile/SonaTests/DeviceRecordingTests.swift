import AVFoundation
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

    /// `calendar_event` is byte for byte `runtime.rs`'s `PHONE_CALENDAR_EVENT`, so a rename
    /// on either side fails a test instead of shipping a recording the desktop refuses.
    func testCalendarEventIsTheAgreedJson() throws {
        let event = DeviceRecordingManifest.CalendarEvent(
            identifier: "event-0001",
            title: "Launch review",
            start_utc_ms: 1_788_304_800_000,
            end_utc_ms: 1_788_308_400_000,
            attendees: [
                .init(name: "Dana Lee", status: 2, is_self: false, url: "mailto:Dana.Lee@Example.com"),
                .init(name: "Aktan Azat", status: 4, is_self: true, url: "mailto:aktan@example.com"),
                .init(name: nil, status: 0, is_self: false, url: "mailto:guest@example.com"),
            ],
            location: "Room 4"
        )
        let manifest = DeviceRecordingObject.manifest(
            deviceId: "phone_device_0001",
            recordedAtUtcMs: 1_700_000_000_000,
            durationMs: 1500,
            title: "Phone recording",
            audioByteLength: 48000,
            audioSha256: "T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8",
            calendarEvent: event
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
            "calendar_event":{"attendees":[\
            {"is_self":false,"name":"Dana Lee","status":2,"url":"mailto:Dana.Lee@Example.com"},\
            {"is_self":true,"name":"Aktan Azat","status":4,"url":"mailto:aktan@example.com"},\
            {"is_self":false,"status":0,"url":"mailto:guest@example.com"}],\
            "end_utc_ms":1788308400000,"identifier":"event-0001",\
            "location":"Room 4","start_utc_ms":1788304800000,\
            "title":"Launch review"},\
            "device_id":"phone_device_0001","duration_ms":1500,"format_version":1,\
            "kind":"device_recording","recorded_at_utc_ms":1700000000000,\
            "title":"Phone recording"}
            """.utf8)
        )
        XCTAssertEqual(
            try JSONDecoder().decode(DeviceRecordingManifest.self, from: encoded), manifest
        )
    }

    /// `bookmark_offsets_ms` is byte for byte `runtime.rs`'s `PHONE_BOOKMARK_OFFSETS`. A
    /// recording with no bookmarks leaves the field out, as the first test pins.
    func testBookmarksAreTheAgreedJson() throws {
        let manifest = DeviceRecordingObject.manifest(
            deviceId: "phone_device_0001",
            recordedAtUtcMs: 1_700_000_000_000,
            durationMs: 1500,
            title: "Phone recording",
            audioByteLength: 48000,
            audioSha256: "T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8",
            bookmarkOffsetsMs: [250, 1040]
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
            "bookmark_offsets_ms":[250,1040],\
            "device_id":"phone_device_0001","duration_ms":1500,"format_version":1,\
            "kind":"device_recording","recorded_at_utc_ms":1700000000000,\
            "title":"Phone recording"}
            """.utf8)
        )
        XCTAssertEqual(
            try JSONDecoder().decode(DeviceRecordingManifest.self, from: encoded), manifest
        )
    }

    /// `calendar_event.place` is byte for byte `runtime.rs`'s `PHONE_PINNED_CALENDAR_EVENT`.
    func testCalendarPinIsTheAgreedJson() throws {
        let event = DeviceRecordingManifest.CalendarEvent(
            identifier: "event-0002",
            title: "Coffee with Dana",
            start_utc_ms: 1_788_304_800_000,
            end_utc_ms: 1_788_308_400_000,
            attendees: [],
            place: .init(
                title: "Hyatt Regency San Francisco\n5 Embarcadero Center",
                latitude: 37.79425,
                longitude: -122.39556
            )
        )
        let manifest = DeviceRecordingObject.manifest(
            deviceId: "phone_device_0001",
            recordedAtUtcMs: 1_700_000_000_000,
            durationMs: 1500,
            title: "Phone recording",
            audioByteLength: 48000,
            audioSha256: "T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8",
            calendarEvent: event
        )
        let encoded = try DeviceRecordingObject.encodeManifest(manifest)
        // As sent: JSONSerialization would print 37.79425 as 37.794249999999998.
        XCTAssertEqual(
            encoded,
            Data("""
            {"audio":{"byte_length":48000,"channels":1,"codec":"pcm_s16le",\
            "sample_rate_hz":16000,"sha256":"T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8"},\
            "calendar_event":{"attendees":[],\
            "end_utc_ms":1788308400000,"identifier":"event-0002",\
            "place":{"latitude":37.79425,"longitude":-122.39556,\
            "title":"Hyatt Regency San Francisco\\n5 Embarcadero Center"},\
            "start_utc_ms":1788304800000,"title":"Coffee with Dana"},\
            "device_id":"phone_device_0001","duration_ms":1500,"format_version":1,\
            "kind":"device_recording","recorded_at_utc_ms":1700000000000,\
            "title":"Phone recording"}
            """.utf8)
        )
        XCTAssertEqual(
            try JSONDecoder().decode(DeviceRecordingManifest.self, from: encoded), manifest
        )
    }

    /// `place_reading` is byte for byte `runtime.rs`'s `PHONE_PLACE_READING`.
    func testPlaceReadingIsTheAgreedJson() throws {
        let manifest = DeviceRecordingObject.manifest(
            deviceId: "phone_device_0001",
            recordedAtUtcMs: 1_700_000_000_000,
            durationMs: 1500,
            title: "Phone recording",
            audioByteLength: 48000,
            audioSha256: "T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8",
            placeReading: .init(latitude: 37.79425, longitude: -122.39556, accuracy_meters: 35)
        )
        let encoded = try DeviceRecordingObject.encodeManifest(manifest)
        // As sent: JSONSerialization would print 37.79425 as 37.794249999999998.
        XCTAssertEqual(
            encoded,
            Data("""
            {"audio":{"byte_length":48000,"channels":1,"codec":"pcm_s16le",\
            "sample_rate_hz":16000,"sha256":"T8pUgTxKXVmy0A-asLSzM7bdweWhqToLZWeaGTYG3_8"},\
            "device_id":"phone_device_0001","duration_ms":1500,"format_version":1,\
            "kind":"device_recording",\
            "place_reading":{"accuracy_meters":35,"latitude":37.79425,"longitude":-122.39556},\
            "recorded_at_utc_ms":1700000000000,"title":"Phone recording"}
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

    /// The upload copy is a tenth of the capture or less, at the capture's rate and
    /// channel count, and as long as the capture give or take the encoder's priming.
    func testUploadCopyIsATenthOfTheCaptureAndAsLong() throws {
        let capture = try writeSpeechLikeCapture(seconds: 60)
        let destination = capture.url.deletingPathExtension().appendingPathExtension("aac")
        addTeardownBlock { try? FileManager.default.removeItem(at: destination) }

        let upload = try encodeForUpload(capture, destination: destination)

        XCTAssertEqual(upload.codec, "aac_adts")
        XCTAssertEqual(upload.durationMs, capture.durationMs)
        let bytes = try Data(contentsOf: destination)
        XCTAssertEqual(Array(bytes.prefix(2)), [0xff, 0xf1], "the Mac's decoder recognizes MPEG-4 ADTS")
        XCTAssertEqual(upload.byteLength, bytes.count)
        XCTAssertEqual(upload.sha256, sha256Base64URL(bytes))
        XCTAssertLessThan(upload.byteLength * 9, capture.byteLength, "\(upload.byteLength) of \(capture.byteLength)")
        XCTAssertTrue(FileManager.default.fileExists(atPath: capture.url.path), "the capture stays for crash recovery")
        let file = try AVAudioFile(forReading: destination)
        XCTAssertEqual(file.fileFormat.streamDescription.pointee.mFormatID, kAudioFormatMPEG4AAC)
        XCTAssertEqual(file.fileFormat.sampleRate, 16_000)
        XCTAssertEqual(file.fileFormat.channelCount, 1)
        let captureFrames = AVAudioFramePosition(capture.byteLength / 2)
        XCTAssertTrue((captureFrames - 1024...captureFrames + 4096).contains(file.length), "\(file.length) frames")
    }

    func testEncodedRecordingManifestNamesItsCodec() throws {
        let manifest = DeviceRecordingObject.manifest(
            deviceId: "phone_device_0001", recordedAtUtcMs: 1, durationMs: 1500,
            title: "Phone recording", audioByteLength: 4500, audioSha256: "sha",
            audioCodec: UploadAudioFormat.codec
        )
        let decoded = try JSONDecoder().decode(
            DeviceRecordingManifest.self, from: DeviceRecordingObject.encodeManifest(manifest)
        )
        XCTAssertEqual(decoded.audio.codec, "aac_adts")
        XCTAssertEqual(decoded.audio.sample_rate_hz, 16000)
        XCTAssertEqual(decoded.audio.channels, 1)
    }

    func testCompactWatchAudioIsNotEncodedTwice() throws {
        let capture = try writeSpeechLikeCapture(seconds: 1)
        let source = capture.url.deletingPathExtension().appendingPathExtension("aac")
        let destination = source.deletingPathExtension().appendingPathExtension("decoded.pcm")
        addTeardownBlock {
            for url in [capture.url, source, destination] { try? FileManager.default.removeItem(at: url) }
        }
        let encoded = try encodeForUpload(capture, destination: source)
        let original = try Data(contentsOf: source)

        let prepared = try prepareWatchAudio(source: source, destination: destination, durationMs: 1000)

        let upload = try XCTUnwrap(prepared.upload)
        XCTAssertEqual(upload.url, source)
        XCTAssertEqual(upload.codec, "aac_adts")
        XCTAssertEqual(upload.sha256, encoded.sha256)
        XCTAssertEqual(upload.durationMs, 1000)
        XCTAssertEqual(try Data(contentsOf: source), original, "not a second lossy encode")
        XCTAssertGreaterThan(prepared.capture.byteLength, 0, "the phone's local library still has decoded audio")
    }

    /// The older phone names every incoming watch file .wav before AVAudioFile opens it.
    /// Its existing decode still reads a new watch's AAC from that name.
    func testAnOlderPhoneCanDecodeACompactWatchFileNamedWave() throws {
        let capture = try writeSpeechLikeCapture(seconds: 1)
        let aac = capture.url.deletingPathExtension().appendingPathExtension("aac")
        let wave = capture.url.deletingPathExtension().appendingPathExtension("wav")
        let decoded = capture.url.deletingPathExtension().appendingPathExtension("decoded.pcm")
        addTeardownBlock {
            for url in [capture.url, aac, wave, decoded] { try? FileManager.default.removeItem(at: url) }
        }
        _ = try encodeForUpload(capture, destination: aac)
        try FileManager.default.moveItem(at: aac, to: wave)

        let prepared = try prepareWatchAudio(source: wave, destination: decoded, durationMs: 1000)

        XCTAssertNil(prepared.upload, "the older phone follows its existing decode-then-encode path")
        XCTAssertGreaterThan(prepared.capture.byteLength, 0)
    }

    func testDecodableWatchAudioOutsideTheUploadFormatIsKeptAndEncoded() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: "watch-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        let source = root.appending(path: "watch.aac")
        let format = try XCTUnwrap(AVAudioFormat(standardFormatWithSampleRate: 22_050, channels: 1))
        let buffer = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 22_050))
        buffer.frameLength = 22_050
        let samples = try XCTUnwrap(buffer.floatChannelData)[0]
        for index in 0..<22_050 {
            samples[index] = 0.25 * sin(Float(index) * 2 * .pi * 300 / 22_050)
        }
        let output = try AVAudioFile(
            forWriting: source, settings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 22_050, AVNumberOfChannelsKey: 1,
                AVEncoderBitRateKey: 24_000,
            ]
        )
        try output.write(from: buffer)
        output.close()

        let prepared = try prepareWatchAudio(
            source: source, destination: root.appending(path: "audio.pcm"), durationMs: 1000
        )

        XCTAssertNil(prepared.upload)
        XCTAssertGreaterThan(prepared.capture.byteLength, 30_000)
        let upload = try encodeForUpload(prepared.capture, destination: root.appending(path: "audio.aac"))
        XCTAssertEqual(upload.codec, "aac_adts")
        XCTAssertEqual(try AVAudioFile(forReading: upload.url).fileFormat.sampleRate, 16_000)
    }

    func testFailedWatchDecodeKeepsItsSourceForRecovery() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: "watch-recovery-\(UUID().uuidString)")
        let source = root.appending(path: "incoming.aac")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: root) }
        let bytes = Data("not a decodable recording".utf8)
        try bytes.write(to: source)
        let journal = try PhoneCaptureJournal.receiveWatch(
            source: source, format: .aac, recordedAtUtcMs: 1234, root: root
        )
        try FileManager.default.removeItem(at: source)
        XCTAssertThrowsError(try journal.recoverAudio(under: root))

        let pending = try PhoneCaptureJournal.pending(under: root)
        XCTAssertEqual(pending, [journal.directory(under: root)])
        let recovered = try PhoneCaptureJournal.load(from: XCTUnwrap(pending.first))
        XCTAssertEqual(recovered.id, journal.id)
        XCTAssertEqual(recovered.recordedAtUtcMs, 1234)
        XCTAssertEqual(recovered.watchSource, .aac)
        XCTAssertEqual(
            try Data(contentsOf: recovered.directory(under: root).appending(path: "watch.aac")),
            bytes
        )
    }
}

/// Capture-format PCM shaped like speech: a gliding voiced tone in syllable-length
/// bursts with some breath noise, deterministic so sizes repeat.
func writeSpeechLikeCapture(seconds: Int) throws -> CapturedAudio {
    let rate = Double(RecordingAudioFormat.sampleRateHz)
    var seed: UInt32 = 0x5EED
    var phase = 0.0
    var pcm = Data(capacity: seconds * RecordingAudioFormat.sampleRateHz * 2)
    for index in 0..<(seconds * RecordingAudioFormat.sampleRateHz) {
        let time = Double(index) / rate
        let pitch = 140 + 60 * sin(2 * .pi * 0.3 * time)
        phase += 2 * .pi * pitch / rate
        let syllable = max(0, sin(2 * .pi * 4 * time))
        var voiced = 0.0
        for harmonic in 1...8 { voiced += sin(phase * Double(harmonic)) / Double(harmonic) }
        seed = seed &* 1_664_525 &+ 1_013_904_223
        let noise = Double(Int32(bitPattern: seed)) / Double(Int32.max)
        let sample = 0.3 * syllable * voiced + 0.02 * noise
        var value = Int16(max(-1, min(1, sample)) * Double(Int16.max)).littleEndian
        withUnsafeBytes(of: &value) { pcm.append(contentsOf: $0) }
    }
    let url = FileManager.default.temporaryDirectory.appending(path: "capture-\(UUID().uuidString).pcm")
    try pcm.write(to: url)
    return CapturedAudio(url: url, byteLength: pcm.count, sha256: sha256Base64URL(pcm),
                         durationMs: Int64(seconds * 1000))
}

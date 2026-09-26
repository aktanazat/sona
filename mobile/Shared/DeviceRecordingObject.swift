import Foundation

/// The plaintext manifest of a `device_recording` vault object.
///
/// Field order is the wire order agreed with the desktop reader; `JSONEncoder` emits
/// stored properties in declaration order, so the encoded bytes stay stable.
struct DeviceRecordingManifest: Codable, Equatable {
    struct Audio: Codable, Equatable {
        var codec: String
        var sample_rate_hz: Int
        var channels: Int
        var byte_length: Int
        var sha256: String
    }

    var format_version: Int
    var kind: String
    var device_id: String
    var recorded_at_utc_ms: Int64
    var duration_ms: Int64
    var title: String
    var audio: Audio
    var personal_notes: String?
}

/// Recording capture format. The phone resamples to this before anything is written,
/// so every byte that leaves the device is already in it.
enum RecordingAudioFormat {
    static let codec = "pcm_s16le"
    static let sampleRateHz = 16000
    static let channels = 1
    static let bytesPerFrame = 2
}

enum DeviceRecordingObject {
    /// The manifest `kind` the desktop reader matches on.
    static let kind = "device_recording"

    /// The `source_format` bound into every payload's HKDF info and AES-GCM AAD.
    ///
    /// crypto v1 requires a non-empty source format but the two authorities only name
    /// `sona-meeting-bundle-json-v1` and `markdown-utf8`; a phone recording is neither,
    /// so this value is introduced here and the desktop reader must use it verbatim.
    static let sourceFormat = "sona-device-recording-v1"

    /// Encrypted chunk ceiling from `client.rs` / `constants.ts`; the plaintext slice is
    /// that minus the 12-byte nonce and 16-byte tag.
    static let maxEncryptedChunkBytes = 4 * 1024 * 1024
    static let maxPlaintextChunkBytes = maxEncryptedChunkBytes - 28

    static func manifest(
        deviceId: String,
        recordedAtUtcMs: Int64,
        durationMs: Int64,
        title: String,
        audioByteLength: Int,
        audioSha256: String,
        notes: String? = nil
    ) -> DeviceRecordingManifest {
        DeviceRecordingManifest(
            format_version: 1,
            kind: kind,
            device_id: deviceId,
            recorded_at_utc_ms: recordedAtUtcMs,
            duration_ms: durationMs,
            title: title,
            audio: DeviceRecordingManifest.Audio(
                codec: RecordingAudioFormat.codec,
                sample_rate_hz: RecordingAudioFormat.sampleRateHz,
                channels: RecordingAudioFormat.channels,
                byte_length: audioByteLength,
                sha256: audioSha256
            ),
            personal_notes: notes?.isEmpty == false ? notes : nil
        )
    }

    /// Encode the manifest with sorted keys.
    ///
    /// JSON objects are unordered and `JSONEncoder`'s field order is not stable across
    /// OS versions, so the one wire form this writer emits is the sorted one; the
    /// desktop reader parses it as JSON and does not depend on order either way.
    static func encodeManifest(_ manifest: DeviceRecordingManifest) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return try encoder.encode(manifest)
    }

    /// Number of chunks the audio splits into, matching the desktop's
    /// `plaintext.chunks(MAX_PLAINTEXT_CHUNK_BYTES)`.
    static func chunkCount(audioByteLength: Int) -> Int {
        max(1, (audioByteLength + maxPlaintextChunkBytes - 1) / maxPlaintextChunkBytes)
    }

    static func chunkRange(index: Int, audioByteLength: Int) -> Range<Int> {
        let start = index * maxPlaintextChunkBytes
        return start..<min(start + maxPlaintextChunkBytes, audioByteLength)
    }
}

/// The `POST /v1/uploads` body. Keys are camelCase because the Worker's request bodies
/// are camelCase while its responses are snake_case.
struct ObjectUploadPlan: Encodable {
    struct Chunk: Encodable {
        var index: Int
        var size: Int
        var sha256: String
    }

    var version: Int
    var cryptoVersion: Int
    var uploadId: String
    var objectId: String
    var revisionId: String
    var manifest: String
    var manifestSha256: String
    var chunks: [Chunk]
    var chunkCount: Int
    var totalBytes: Int
    var writerSignature: String
}

/// Stable idempotency key: base64url(sha256(part || 0x00 …)), as `runtime.rs` derives it.
func stableIdempotencyKey(_ parts: [String]) -> String {
    var bytes = Data()
    for part in parts {
        bytes.append(Data(part.utf8))
        bytes.append(0)
    }
    return Base64URL.encode(sha256Digest(bytes))
}

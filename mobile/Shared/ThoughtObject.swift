import Foundation

/// The plaintext manifest of a `thought` vault object: what the operator captured,
/// with every attachment described by the chunks that carry its bytes.
///
/// Field names are the wire names the sorter's `ThoughtManifest` schema parses.
struct ThoughtManifest: Codable, Equatable {
    struct Audio: Codable, Equatable {
        var codec: String
        var sample_rate_hz: Int
        var channels: Int
        var byte_length: Int
        var sha256: String
        var duration_ms: Int64
        var chunk_start: Int
        var chunk_count: Int
    }

    struct Image: Codable, Equatable {
        var mime: String
        var byte_length: Int
        var sha256: String
        var width: Int
        var height: Int
        var chunk_start: Int
        var chunk_count: Int
    }

    struct Link: Codable, Equatable {
        var url: String
    }

    var format_version: Int
    var kind: String
    var device_id: String
    var captured_at_utc_ms: Int64
    var origin: String
    var text: String
    var link: Link?
    var audio: Audio?
    var images: [Image]

    /* The sorter's schema takes `link` and `audio` as nullable, not optional: the key
     * has to be there. Synthesized encoding would drop a nil key, so this writes every
     * field and lets a nil come out as JSON null. */
    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(format_version, forKey: .format_version)
        try container.encode(kind, forKey: .kind)
        try container.encode(device_id, forKey: .device_id)
        try container.encode(captured_at_utc_ms, forKey: .captured_at_utc_ms)
        try container.encode(origin, forKey: .origin)
        try container.encode(text, forKey: .text)
        try container.encode(link, forKey: .link)
        try container.encode(audio, forKey: .audio)
        try container.encode(images, forKey: .images)
    }
}

/// How a thought was captured; the sorter reads the raw value.
enum ThoughtOrigin: String, Codable {
    case voice
    case typed
    case shared
}

/// One attachment before it is sealed: where its bytes are and what they are.
struct ThoughtAttachment: Codable, Equatable {
    enum Kind: Codable, Equatable {
        case audio(durationMs: Int64)
        case image(mime: String, width: Int, height: Int)
    }

    var kind: Kind
    /// File name inside the outbox item directory.
    var file: String
    var byteLength: Int
    var sha256: String
}

/// Where one attachment's bytes sit in a revision's chunk sequence.
struct ThoughtChunkSpan: Equatable {
    var attachment: Int
    var start: Int
    var count: Int
}

enum ThoughtObject {
    static let kind = "thought"

    /// The `source_format` bound into every payload's HKDF info and AES-GCM AAD; the
    /// sorter and the board open a manifest under this value and no other.
    static let sourceFormat = "sona-thought-v1"

    /// Attachments occupy consecutive chunk ranges in attachment order, each sliced at
    /// the crypto v1 chunk ceiling. Crypto v1 also requires at least one chunk, so a
    /// thought with nothing attached carries one empty chunk.
    static func spans(_ attachments: [ThoughtAttachment]) -> [ThoughtChunkSpan] {
        var next = 0
        return attachments.enumerated().map { index, attachment in
            let count = DeviceRecordingObject.chunkCount(audioByteLength: attachment.byteLength)
            defer { next += count }
            return ThoughtChunkSpan(attachment: index, start: next, count: count)
        }
    }

    static func chunkCount(_ attachments: [ThoughtAttachment]) -> Int {
        max(1, spans(attachments).reduce(0) { $0 + $1.count })
    }

    /// The attachment and byte range chunk `index` carries, or nil for the one empty
    /// chunk of an attachment-free thought.
    static func slice(
        _ attachments: [ThoughtAttachment], index: Int
    ) -> (attachment: ThoughtAttachment, range: Range<Int>)? {
        guard let span = spans(attachments).first(where: {
            $0.start <= index && index < $0.start + $0.count
        }) else { return nil }
        let attachment = attachments[span.attachment]
        let range = DeviceRecordingObject.chunkRange(
            index: index - span.start, audioByteLength: attachment.byteLength
        )
        return (attachment, range)
    }

    static func manifest(
        deviceId: String,
        capturedAtUtcMs: Int64,
        origin: ThoughtOrigin,
        text: String,
        link: URL?,
        attachments: [ThoughtAttachment]
    ) -> ThoughtManifest {
        var audio: ThoughtManifest.Audio?
        var images: [ThoughtManifest.Image] = []
        for span in spans(attachments) {
            let attachment = attachments[span.attachment]
            switch attachment.kind {
            case let .audio(durationMs):
                audio = ThoughtManifest.Audio(
                    codec: RecordingAudioFormat.codec,
                    sample_rate_hz: RecordingAudioFormat.sampleRateHz,
                    channels: RecordingAudioFormat.channels,
                    byte_length: attachment.byteLength,
                    sha256: attachment.sha256,
                    duration_ms: durationMs,
                    chunk_start: span.start,
                    chunk_count: span.count
                )
            case let .image(mime, width, height):
                images.append(
                    ThoughtManifest.Image(
                        mime: mime,
                        byte_length: attachment.byteLength,
                        sha256: attachment.sha256,
                        width: width,
                        height: height,
                        chunk_start: span.start,
                        chunk_count: span.count
                    )
                )
            }
        }
        return ThoughtManifest(
            format_version: 1,
            kind: kind,
            device_id: deviceId,
            captured_at_utc_ms: capturedAtUtcMs,
            origin: origin.rawValue,
            text: text,
            link: link.map { ThoughtManifest.Link(url: $0.absoluteString) },
            audio: audio,
            images: images
        )
    }

    /// Sorted keys, for the same reason `DeviceRecordingObject.encodeManifest` sorts.
    static func encodeManifest(_ manifest: ThoughtManifest) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return try encoder.encode(manifest)
    }
}

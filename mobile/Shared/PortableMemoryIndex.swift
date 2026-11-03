import Foundation

struct PortableMemoryIndex: Codable, Equatable {
    static let sourceFormat = "sona-memory-index-v1"
    static let kind = "memory_index"
    static let maxPhotoBytes = 1024 * 1024

    struct Manifest: Decodable {
        var kind: String
    }

    struct Person: Codable, Equatable, Identifiable {
        var id: String
        var display_name: String
        var photoPNGData: Data?

        enum CodingKeys: String, CodingKey {
            case id, display_name
            case photoPNGData = "photo_png_base64"
        }
    }

    struct Source: Codable, Equatable {
        var kind: String
        var id: String
        var title: String
        var available: Bool
    }

    struct Note: Codable, Equatable, Identifiable {
        var id: String
        var title: String
        var body: String
        var updated_at_ms: Int64
        var sources: [Source]
        var updatedAt: Date { Date(timeIntervalSince1970: Double(updated_at_ms) / 1000) }
    }

    struct Attribution: Codable, Equatable, Hashable {
        var meeting_id: String
        var speaker_id: String
        var person_id: String
    }

    struct Bookmark: Codable, Equatable {
        var meeting_id: String
        var start_offset_ns: UInt64
        var label: String?
    }

    var format_version: Int
    var kind: String
    var source_device_id: String
    var produced_at_utc_ms: Int64
    var people: [Person]
    var notes: [Note]
    var attributions: [Attribution]
    var bookmarks: [Bookmark]
    var producedAt: Date { Date(timeIntervalSince1970: Double(produced_at_utc_ms) / 1000) }

    enum CodingKeys: String, CodingKey {
        case format_version, kind, source_device_id, produced_at_utc_ms
        case people, notes, attributions, bookmarks
    }
}

extension PortableMemoryIndex.Person {
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        id = try values.decode(String.self, forKey: .id)
        display_name = try values.decode(String.self, forKey: .display_name)
        photoPNGData = nil
        if let encoded = try values.decodeIfPresent(String.self, forKey: .photoPNGData),
           encoded.utf8.count <= ((PortableMemoryIndex.maxPhotoBytes + 2) / 3) * 4 {
            guard let bytes = Data(base64Encoded: encoded),
                  bytes.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) else {
                throw DecodingError.dataCorruptedError(forKey: .photoPNGData, in: values,
                                                       debugDescription: "Expected a base64 PNG portrait")
            }
            if bytes.count <= PortableMemoryIndex.maxPhotoBytes { photoPNGData = bytes }
        }
    }
}

extension PortableMemoryIndex {
    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        format_version = try values.decode(Int.self, forKey: .format_version)
        kind = try values.decode(String.self, forKey: .kind)
        source_device_id = try values.decode(String.self, forKey: .source_device_id)
        produced_at_utc_ms = try values.decode(Int64.self, forKey: .produced_at_utc_ms)
        people = try values.decode([Person].self, forKey: .people)
        notes = try values.decode([Note].self, forKey: .notes)
        attributions = try values.decode([Attribution].self, forKey: .attributions)
        bookmarks = try values.decode([Bookmark].self, forKey: .bookmarks)
        let personIDs = Set(people.map(\.id))
        let assignments = Dictionary(grouping: attributions) { [$0.meeting_id, $0.speaker_id] }
        guard format_version == 1, kind == Self.kind, !source_device_id.isEmpty,
              personIDs.count == people.count,
              people.allSatisfy({ !$0.id.isEmpty && !$0.display_name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }),
              Set(notes.map(\.id)).count == notes.count,
              notes.allSatisfy({ !$0.id.isEmpty && $0.sources.allSatisfy { !$0.kind.isEmpty && !$0.id.isEmpty } }),
              attributions.allSatisfy({ !$0.meeting_id.isEmpty && !$0.speaker_id.isEmpty && personIDs.contains($0.person_id) }),
              assignments.values.allSatisfy({ Set($0.map(\.person_id)).count == 1 }),
              bookmarks.allSatisfy({ !$0.meeting_id.isEmpty }) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                                                    debugDescription: "Invalid memory index snapshot"))
        }
    }
}

import Foundation

enum PersonPhotoProvenance: Codable, Equatable {
    case localFile(fileName: String, confirmedAtUtcMs: Int64)
    case localContact(contactName: String, confirmedAtUtcMs: Int64)
    case publicImage(sourceURL: String, imageURL: String, attribution: String, confirmedAtUtcMs: Int64)

    private enum Keys: String, CodingKey {
        case kind, fileName, contactName, sourceUrl, imageUrl, attribution, confirmedAtUtcMs
    }

    private enum WireKeys: String, CodingKey {
        case kind, attribution
        case fileName = "file_name"
        case contactName = "contact_name"
        case sourceURL = "source_url"
        case imageURL = "image_url"
        case confirmedAtUtcMs = "confirmed_at_utc_ms"
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: Keys.self)
        let confirmed = try values.decode(Int64.self, forKey: .confirmedAtUtcMs)
        switch try values.decode(String.self, forKey: .kind) {
        case "local_file":
            self = .localFile(fileName: try values.decode(String.self, forKey: .fileName), confirmedAtUtcMs: confirmed)
        case "local_contact":
            self = .localContact(contactName: try values.decode(String.self, forKey: .contactName), confirmedAtUtcMs: confirmed)
        case "public_image":
            self = .publicImage(
                sourceURL: try values.decode(String.self, forKey: .sourceUrl),
                imageURL: try values.decode(String.self, forKey: .imageUrl),
                attribution: try values.decode(String.self, forKey: .attribution),
                confirmedAtUtcMs: confirmed)
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: values, debugDescription: "Unknown photo source")
        }
    }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: WireKeys.self)
        switch self {
        case let .localFile(name, confirmed):
            try values.encode("local_file", forKey: .kind)
            try values.encode(name, forKey: .fileName)
            try values.encode(confirmed, forKey: .confirmedAtUtcMs)
        case let .localContact(name, confirmed):
            try values.encode("local_contact", forKey: .kind)
            try values.encode(name, forKey: .contactName)
            try values.encode(confirmed, forKey: .confirmedAtUtcMs)
        case let .publicImage(source, image, attribution, confirmed):
            try values.encode("public_image", forKey: .kind)
            try values.encode(source, forKey: .sourceURL)
            try values.encode(image, forKey: .imageURL)
            try values.encode(attribution, forKey: .attribution)
            try values.encode(confirmed, forKey: .confirmedAtUtcMs)
        }
    }

    var label: String {
        switch self {
        case let .localFile(name, _): "File · \(name)"
        case let .localContact(name, _): "Contacts · \(name)"
        case let .publicImage(_, _, attribution, _): attribution
        }
    }

    var sourceURL: URL? {
        if case let .publicImage(source, _, _, _) = self { return URL(string: source) }
        return nil
    }

    var confirmedAt: Date {
        let milliseconds: Int64
        switch self {
        case let .localFile(_, date), let .localContact(_, date), let .publicImage(_, _, _, date):
            milliseconds = date
        }
        return Date(timeIntervalSince1970: Double(milliseconds) / 1000)
    }

    func confirmed(at milliseconds: Int64) -> Self {
        switch self {
        case let .localFile(name, _): .localFile(fileName: name, confirmedAtUtcMs: milliseconds)
        case let .localContact(name, _): .localContact(contactName: name, confirmedAtUtcMs: milliseconds)
        case let .publicImage(source, image, attribution, _):
            .publicImage(sourceURL: source, imageURL: image, attribution: attribution, confirmedAtUtcMs: milliseconds)
        }
    }
}

struct PersonAvatarSourceResult: Decodable {
    let originalBase64: String?
    let provenance: PersonPhotoProvenance?
}

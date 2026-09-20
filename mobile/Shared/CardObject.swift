import Foundation

/// The plaintext manifest of a `card` vault object: what the sorter made of one
/// thought. The phone only reads these; the sorter is their writer.
struct CardManifest: Codable, Equatable {
    struct Cluster: Codable, Equatable, Hashable {
        var key: String
        var name: String
        /// 0...359, the cluster's colour on the board.
        var hue: Int
    }

    struct Writer: Codable, Equatable {
        var device_id: String
        var role: String
    }

    struct Sorter: Codable, Equatable {
        var model: String
        var prompt_version: Int
    }

    var format_version: Int
    var kind: String
    var thought_id: String
    var thought_revision_id: String
    var title: String
    var summary: String
    var tags: [String]
    var cluster: Cluster
    var written_at_utc_ms: Int64
    var writer: Writer
    var sorter: Sorter?
    var pinned: Bool
    var archived: Bool
}

enum CardObject {
    static let kind = "card"

    /// The `source_format` the sorter seals a card under.
    static let sourceFormat = "sona-card-v1"
}

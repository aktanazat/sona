import Foundation

/// What the board knows about one object in the vault.
enum BoardObject: Codable, Equatable {
    case thought(ThoughtManifest)
    case card(CardManifest)
    /// A meeting bundle, a recording, or a format this build does not know.
    case other
}

struct BoardHead: Codable, Equatable {
    var revisionId: String
    /// The revision's chunk count, which every chunk's key and AAD bind.
    var chunkCount: Int
    var object: BoardObject
}

/// The change feed folded into heads, keyed by object id. Pure: the reader decides
/// what to fetch, this decides what the fetch means.
struct BoardIndex: Codable, Equatable {
    var vaultId: String
    var cursor: String?
    var heads: [String: BoardHead] = [:]

    /// Whether `change` names a revision the index has not read yet.
    func needsRead(_ change: ChangeRow) -> Bool {
        !change.tombstone && heads[change.objectId]?.revisionId != change.revisionId
    }

    mutating func remove(_ objectId: String) {
        heads[objectId] = nil
    }

    mutating func set(_ change: ChangeRow, chunkCount: Int, object: BoardObject) {
        heads[change.objectId] = BoardHead(
            revisionId: change.revisionId, chunkCount: chunkCount, object: object
        )
    }

    var thoughts: [(id: String, head: BoardHead, thought: ThoughtManifest)] {
        heads.compactMap { id, head in
            guard case let .thought(thought) = head.object else { return nil }
            return (id, head, thought)
        }
    }

    var cards: [(id: String, head: BoardHead, card: CardManifest)] {
        heads.compactMap { id, head in
            guard case let .card(card) = head.object else { return nil }
            return (id, head, card)
        }
    }
}

/// One tile on the board: a thought and the card the sorter wrote about it, if any.
struct BoardTile: Identifiable, Equatable {
    /// The thought's object id.
    var id: String
    var head: BoardHead
    var thought: ThoughtManifest
    var card: CardManifest?

    /// The card's title, else the thought's first line; nil when it has no words.
    var title: String? {
        if let card { return card.title }
        let line = thought.text
            .split(whereSeparator: { $0.isNewline })
            .first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
        return line.isEmpty ? nil : line
    }
}

/// One group of tiles: a cluster the sorter named, or the thoughts still waiting for one.
struct BoardSection: Identifiable, Equatable {
    /// The cluster key, or empty for the unsorted group.
    var id: String
    var cluster: CardManifest.Cluster?
    var tiles: [BoardTile]
}

extension BoardIndex {
    /// The board: unsorted thoughts first, then clusters by name. Inside a section,
    /// pinned tiles lead and the rest run newest first. An archived card hides its
    /// thought; the newest card about a thought is the one that counts.
    func sections() -> [BoardSection] {
        var newest: [String: CardManifest] = [:]
        for (_, _, card) in cards {
            if let known = newest[card.thought_id], known.written_at_utc_ms >= card.written_at_utc_ms {
                continue
            }
            newest[card.thought_id] = card
        }
        var grouped: [String: BoardSection] = [:]
        for (id, head, thought) in thoughts {
            let card = newest[id]
            if card?.archived == true { continue }
            let key = card?.cluster.key ?? ""
            var section = grouped[key] ?? BoardSection(id: key, cluster: card?.cluster, tiles: [])
            section.tiles.append(BoardTile(id: id, head: head, thought: thought, card: card))
            grouped[key] = section
        }
        return grouped.values
            .map { (section: BoardSection) -> BoardSection in
                var sorted = section
                sorted.tiles.sort(by: BoardIndex.leads)
                return sorted
            }
            .sorted(by: BoardIndex.leads)
    }

    /// Pinned before unpinned, then newest first.
    private static func leads(_ a: BoardTile, _ b: BoardTile) -> Bool {
        let pinnedA = a.card?.pinned ?? false
        let pinnedB = b.card?.pinned ?? false
        if pinnedA != pinnedB { return pinnedA }
        return a.thought.captured_at_utc_ms > b.thought.captured_at_utc_ms
    }

    /// The unsorted group first, then clusters by name.
    private static func leads(_ a: BoardSection, _ b: BoardSection) -> Bool {
        switch (a.cluster, b.cluster) {
        case (nil, _): return true
        case (_, nil): return false
        case let (x?, y?): return x.name == y.name ? x.key < y.key : x.name < y.name
        }
    }
}

enum VaultReaderError: Error, Equatable {
    case notPaired
    case integrity
}

/// The phone's read side of the vault: the change feed kept on disk as a head index,
/// and the bytes of one attachment when a screen asks for them.
actor VaultReader {
    private let file: URL
    private let identity: DeviceIdentity
    private var credentials: VaultCredentials?
    private var client: CompanionClient?
    private var index: BoardIndex?
    /// The fold in flight, so a delete lands after it rather than under it.
    private var syncing: Task<BoardIndex, Error>?

    init(root: URL, identity: DeviceIdentity, credentials: VaultCredentials?) {
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        file = root.appending(path: "index.json")
        self.identity = identity
        self.credentials = credentials
        if let bytes = try? Data(contentsOf: file),
           let stored = try? JSONDecoder().decode(BoardIndex.self, from: bytes),
           stored.vaultId == credentials?.vaultId
        {
            index = stored
        }
    }

    func setCredentials(_ value: VaultCredentials?) {
        credentials = value
        client = nil
        if index?.vaultId != value?.vaultId { index = nil }
    }

    func current() -> BoardIndex? { index }

    /// Fold every change since the cursor into the index, one saved page at a time.
    func sync() async throws -> BoardIndex {
        if let syncing { return try await syncing.value }
        let task = Task { try await fold() }
        syncing = task
        defer { syncing = nil }
        return try await task.value
    }

    private func fold() async throws -> BoardIndex {
        let (credentials, client) = try session()
        var index = self.index ?? BoardIndex(vaultId: credentials.vaultId)
        while true {
            let page: ChangesPage
            do {
                page = try await client.changes(
                    identity: identity, vaultId: credentials.vaultId, cursor: index.cursor
                )
            } catch CompanionError.api(code: "cursor_expired", status: _) {
                index = try await rebuild(index, credentials: credentials, client: client)
                continue
            }
            for change in page.changes {
                if change.tombstone {
                    index.remove(change.objectId)
                } else if index.needsRead(change) {
                    let (chunkCount, object) = try await read(
                        change, credentials: credentials, client: client
                    )
                    index.set(change, chunkCount: chunkCount, object: object)
                }
            }
            index.cursor = page.nextCursor
            try store(index)
            if !page.hasMore { return index }
        }
    }

    /// One attachment's bytes, opened chunk by chunk under the thought's revision.
    func attachment(
        objectId: String, head: BoardHead, chunkStart: Int, chunkCount: Int, sha256: String
    ) async throws -> Data {
        let (credentials, client) = try session()
        var bytes = Data()
        for index in chunkStart..<(chunkStart + chunkCount) {
            let sealed = try await client.chunk(
                identity: identity, vaultId: credentials.vaultId,
                objectId: objectId, revisionId: head.revisionId, index: index
            )
            bytes.append(
                try openObjectRevisionPayload(
                    vaultRoot: credentials.vaultRoot,
                    context: ObjectRevisionCryptoContext(
                        vaultId: credentials.vaultId,
                        objectId: objectId,
                        revisionId: head.revisionId,
                        index: UInt64(index),
                        total: UInt64(head.chunkCount),
                        contentKind: .chunk,
                        sourceFormat: ThoughtObject.sourceFormat
                    ),
                    encryptedPayload: sealed
                )
            )
        }
        guard sha256Base64URL(bytes) == sha256 else { throw VaultReaderError.integrity }
        return bytes
    }

    /// Tombstone a thought and every card written about it, cards first so a failure
    /// part way leaves a thought the sorter files again rather than orphaned cards.
    /// Waits for a sync in flight: its copy of the index would put the heads back.
    func deleteThought(_ id: String) async throws {
        if let syncing { _ = try? await syncing.value }
        guard let index else { return }
        for card in index.cards where card.card.thought_id == id {
            try await tombstone(objectId: card.id)
        }
        try await tombstone(objectId: id)
    }

    /// Tombstone one object over the head this index holds, and drop it locally.
    private func tombstone(objectId: String) async throws {
        let (credentials, client) = try session()
        guard var index, let head = index.heads[objectId] else { return }
        let tombstoneRevisionId = randomOpaqueId()
        _ = try await client.tombstone(
            identity: identity,
            vaultId: credentials.vaultId,
            idempotencyKey: stableIdempotencyKey([
                "tombstone", objectId, head.revisionId, tombstoneRevisionId,
            ]),
            objectId: objectId,
            baseRevisionId: head.revisionId,
            tombstoneRevisionId: tombstoneRevisionId
        )
        index.remove(objectId)
        try store(index)
    }

    // MARK: - Internals

    private func session() throws -> (VaultCredentials, CompanionClient) {
        guard let credentials else { throw VaultReaderError.notPaired }
        let client = try self.client ?? CompanionClient(endpoint: credentials.endpoint)
        self.client = client
        return (credentials, client)
    }

    /// Replace the heads with the Worker's at one high water, then resume the change
    /// feed right after it. The cursor is built in the Worker's own encoding because
    /// the snapshot reply carries no resume cursor.
    private func rebuild(
        _ old: BoardIndex, credentials: VaultCredentials, client: CompanionClient
    ) async throws -> BoardIndex {
        var index = BoardIndex(vaultId: credentials.vaultId)
        var highWater: String?
        var after: String?
        while true {
            let page = try await client.snapshot(
                identity: identity, vaultId: credentials.vaultId,
                highWater: highWater, after: after
            )
            highWater = page.highWater
            for head in page.heads where !head.tombstone {
                if let known = old.heads[head.objectId], known.revisionId == head.revisionId {
                    index.heads[head.objectId] = known
                } else {
                    let (chunkCount, object) = try await read(
                        head, credentials: credentials, client: client
                    )
                    index.set(head, chunkCount: chunkCount, object: object)
                }
            }
            after = page.after
            if !page.hasMore { break }
        }
        guard let highWater, let sequence = VaultReader.snapshotSequence(highWater) else {
            throw VaultReaderError.integrity
        }
        index.cursor = VaultReader.changeCursor(after: sequence)
        try store(index)
        return index
    }

    /// Open a head's manifest as a thought, a card, or something else in the vault.
    ///
    /// `source_format` never travels on the wire, so the reader tries the two formats
    /// it knows and lets AES-GCM answer: a payload opens under exactly the format its
    /// writer sealed it with.
    private func read(
        _ head: ChangeRow, credentials: VaultCredentials, client: CompanionClient
    ) async throws -> (Int, BoardObject) {
        let response = try await client.manifest(
            identity: identity, vaultId: credentials.vaultId,
            objectId: head.objectId, revisionId: head.revisionId
        )
        let envelope = response.envelope
        guard envelope.objectId == head.objectId,
              envelope.revisionId == head.revisionId,
              envelope.cryptoVersion == SonaProtocol.cryptoVersion,
              let sealed = Base64URL.decode(response.manifest),
              sha256Base64URL(sealed) == envelope.manifestSha256
        else { throw VaultReaderError.integrity }
        func open(_ sourceFormat: String) -> Data? {
            try? openObjectRevisionPayload(
                vaultRoot: credentials.vaultRoot,
                context: ObjectRevisionCryptoContext(
                    vaultId: credentials.vaultId,
                    objectId: head.objectId,
                    revisionId: head.revisionId,
                    index: 0,
                    total: UInt64(envelope.chunkCount),
                    contentKind: .manifest,
                    sourceFormat: sourceFormat
                ),
                encryptedPayload: sealed
            )
        }
        let decoder = JSONDecoder()
        if let plaintext = open(ThoughtObject.sourceFormat),
           let thought = try? decoder.decode(ThoughtManifest.self, from: plaintext),
           thought.kind == ThoughtObject.kind, thought.format_version == 1
        {
            return (envelope.chunkCount, .thought(thought))
        }
        if let plaintext = open(CardObject.sourceFormat),
           let card = try? decoder.decode(CardManifest.self, from: plaintext),
           card.kind == CardObject.kind, card.format_version == 1
        {
            return (envelope.chunkCount, .card(card))
        }
        return (envelope.chunkCount, .other)
    }

    private func store(_ index: BoardIndex) throws {
        self.index = index
        try JSONEncoder().encode(index).write(to: file, options: .atomic)
    }

    /// The sequence a snapshot `high_water` token names: `h.` + base64url of
    /// `{"v":1,"w":<sequence>}`.
    static func snapshotSequence(_ token: String) -> Int64? {
        guard token.hasPrefix("h."),
              let bytes = Base64URL.decode(String(token.dropFirst(2))),
              let decoded = try? JSONDecoder().decode(SnapshotHighWater.self, from: bytes),
              decoded.v == 1
        else { return nil }
        return decoded.w
    }

    /// The change cursor after `sequence`, in the Worker's own `c.` encoding.
    static func changeCursor(after sequence: Int64) -> String {
        "c." + Base64URL.encode(Data("{\"v\":1,\"a\":\(sequence)}".utf8))
    }

    private struct SnapshotHighWater: Decodable {
        var v: Int
        var w: Int64
    }
}

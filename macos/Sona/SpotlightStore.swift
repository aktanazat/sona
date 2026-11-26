import AppIntents
import CoreSpotlight
import CryptoKit

/// Keeps Spotlight in step with the people, meetings, tasks, projects and
/// notes the core lists to the system. A meeting of a series kept on this Mac
/// never reaches the index, nor a task drawn from one, nor a personal
/// document; one indexed before it was kept is taken back out.
@MainActor
final class SpotlightStore {
    private enum Part {
        case meetings, work
    }

    private let core: Core
    private let index = CSSearchableIndex(name: "Sona")
    private let stampFile: URL?
    /// What the index holds, kept across launches so a launch writes only
    /// the rows that changed since the last run.
    private var stamp: SpotlightStamp
    private var syncing = false
    /// What changed since the last read. A meeting change marks the work too:
    /// keeping a series on this Mac hides the tasks drawn from it.
    private var due: Set<Part> = []
    private var pending: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        stampFile = try? FileManager.default.url(
            for: .cachesDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        ).appending(path: "Sona/spotlight-index.json")
        stamp = stampFile.flatMap(SpotlightStamp.load) ?? SpotlightStamp()
        for name in [CoreEvent.meetingArtifactChanged, CoreEvent.meetingSessionChanged, CoreEvent.meetingRemoved] {
            core.observe(name) { [weak self] _ in self?.refresh() }
        }
        for name in ["work-changed", "planning-changed-event", CoreEvent.scratchpadChanged] {
            core.observe(name) { [weak self] _ in self?.refresh([.work]) }
        }
    }

    /// The stamp holds only while the index still carries the mark written
    /// with it: a rebuilt or erased index has none, and is written whole.
    func start() async {
        if (try? await index.fetchLastClientState()) != stamp.mark {
            stamp = SpotlightStamp()
        }
        due = [.meetings, .work]
        await sync()
    }

    /// Changes arrive in bursts, so the index is read once after the last.
    func refresh() {
        refresh([.meetings, .work])
    }

    private func refresh(_ parts: Set<Part>) {
        due.formUnion(parts)
        pending?.cancel()
        pending = Task { [weak self] in
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled else { return }
            await self?.sync()
        }
    }

    private func sync() async {
        guard !syncing else { return }
        syncing = true
        let before = stamp
        while !due.isEmpty {
            let parts = due
            due = []
            if parts.contains(.meetings) { await reindexMeetings() }
            if parts.contains(.work) { await reindexWork() }
        }
        if stamp != before, let stampFile {
            let mark = Data(UUID().uuidString.utf8)
            index.beginBatch()
            do {
                try await index.endBatch(withClientState: mark)
                stamp.mark = mark
                try stamp.save(to: stampFile)
            } catch {
                try? FileManager.default.removeItem(at: stampFile)
            }
        }
        syncing = false
    }

    private func reindexMeetings() async {
        guard let list: SystemIndex = try? await core.request("sona_query_system_index") else { return }
        let peopleBefore = stamp.kinds[SpotlightStamp.kind(PersonEntity.self)]
        do {
            try await write(list.people, as: PersonEntity.init)
            try await write(list.meetings, as: MeetingEntity.init)
        } catch {
            stamp.forget(PersonEntity.self)
            stamp.forget(MeetingEntity.self)
            return
        }
        if stamp.kinds[SpotlightStamp.kind(PersonEntity.self)] != peopleBefore {
            SonaShortcuts.updateAppShortcutParameters()
        }
    }

    private func reindexWork() async {
        guard let list: SystemWork = try? await core.request("sona_query_system_work") else { return }
        do {
            try await write(list.tasks, as: TaskEntity.init)
            try await write(list.projects, as: ProjectEntity.init)
            try await write(list.notes, as: NoteEntity.init)
        } catch {
            stamp.forget(TaskEntity.self)
            stamp.forget(ProjectEntity.self)
            stamp.forget(NoteEntity.self)
        }
    }

    private func write<Row: Encodable & Identifiable, Entity: IndexedEntity>(
        _ rows: [Row], as entity: (Row) -> Entity
    ) async throws where Row.ID == String, Entity.ID == String {
        let byId = Dictionary(rows.map { ($0.id, $0) }) { first, _ in first }
        let digests = SpotlightStamp.digests(byId)
        let change = stamp.change(to: digests, for: Entity.self)
        switch change.gone {
        case nil: try await index.deleteAppEntities(ofType: Entity.self)
        case let gone? where !gone.isEmpty:
            try await index.deleteAppEntities(identifiedBy: gone, ofType: Entity.self)
        default: break
        }
        if !change.written.isEmpty {
            try await index.indexAppEntities(change.written.compactMap { byId[$0].map(entity) })
        }
        stamp.kinds[SpotlightStamp.kind(Entity.self)] = digests
    }
}

/// One digest per indexed row, by entity kind and id.
struct SpotlightStamp: Codable, Equatable {
    /// Raise when what an entity puts in the index changes, so every row is
    /// written again once.
    static let currentVersion = 1

    var version = currentVersion
    var kinds: [String: [String: String]] = [:]
    /// The client state written to the index with this stamp.
    var mark: Data?

    /// A missing, unreadable or older stamp reads as none, so the index is
    /// replaced whole.
    static func load(from url: URL) -> SpotlightStamp? {
        guard let data = try? Data(contentsOf: url),
              let stamp = try? JSONDecoder().decode(SpotlightStamp.self, from: data),
              stamp.version == currentVersion else { return nil }
        return stamp
    }

    func save(to url: URL) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(self).write(to: url, options: .atomic)
    }

    static func kind<Entity>(_ entity: Entity.Type) -> String {
        String(describing: entity)
    }

    static func digests<Row: Encodable>(_ rows: [String: Row]) -> [String: String] {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        return rows.compactMapValues { row in
            (try? encoder.encode(row)).map { Data(SHA256.hash(data: $0)).base64EncodedString() }
        }
    }

    /// Ids to take out of the index (nil: every row of the kind, since what
    /// it holds is unknown) and ids to write.
    func change<Entity>(to digests: [String: String], for entity: Entity.Type) -> (gone: [String]?, written: [String]) {
        guard let indexed = kinds[Self.kind(entity)] else { return (nil, Array(digests.keys)) }
        let gone = indexed.keys.filter { digests[$0] == nil }
        let written = digests.compactMap { id, digest in indexed[id] == digest ? nil : id }
        return (gone, written)
    }

    mutating func forget<Entity>(_ entity: Entity.Type) {
        kinds[Self.kind(entity)] = nil
    }
}

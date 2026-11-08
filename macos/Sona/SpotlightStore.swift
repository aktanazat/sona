import AppIntents
import CoreSpotlight

/// Keeps Spotlight in step with the people and meetings the core lists to
/// the system. A meeting of a series kept on this Mac never reaches the
/// index, and one indexed before its series was kept is taken back out.
@MainActor
final class SpotlightStore {
    private let core: Core
    private let index = CSSearchableIndex(name: "Sona")
    /// What the index holds, by id. Nil until the first write since launch,
    /// which replaces whatever an earlier run left.
    private var people: [String: SystemPerson]?
    private var meetings: [MeetingSessionId: SystemMeeting]?
    private var syncing = false
    private var again = false
    private var pending: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        for name in [CoreEvent.meetingArtifactChanged, CoreEvent.meetingSessionChanged, CoreEvent.meetingRemoved] {
            core.observe(name) { [weak self] _ in self?.refresh() }
        }
    }

    func start() async {
        await sync()
    }

    /// Changes arrive in bursts, so the index is read once after the last.
    func refresh() {
        pending?.cancel()
        pending = Task { [weak self] in
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled else { return }
            await self?.sync()
        }
    }

    private func sync() async {
        guard !syncing else {
            again = true
            return
        }
        syncing = true
        repeat {
            again = false
            await reindex()
        } while again
        syncing = false
    }

    private func reindex() async {
        guard let list: SystemIndex = try? await core.request("sona_query_system_index") else { return }
        let people = Dictionary(list.people.map { ($0.id, $0) }) { first, _ in first }
        let meetings = Dictionary(list.meetings.map { ($0.id, $0) }) { first, _ in first }
        do {
            try await write(people, over: self.people, as: PersonEntity.init)
            try await write(meetings, over: self.meetings, as: MeetingEntity.init)
        } catch {
            self.people = nil
            self.meetings = nil
            return
        }
        if self.people != people {
            SonaShortcuts.updateAppShortcutParameters()
        }
        self.people = people
        self.meetings = meetings
    }

    private func write<Row: Equatable, Entity: IndexedEntity>(
        _ rows: [String: Row], over indexed: [String: Row]?, as entity: (Row) -> Entity
    ) async throws where Entity.ID == String {
        if let indexed {
            let gone = indexed.keys.filter { rows[$0] == nil }
            if !gone.isEmpty {
                try await index.deleteAppEntities(identifiedBy: gone, ofType: Entity.self)
            }
        } else {
            try await index.deleteAppEntities(ofType: Entity.self)
        }
        let changed = rows.compactMap { id, row in indexed?[id] == row ? nil : entity(row) }
        if !changed.isEmpty {
            try await index.indexAppEntities(changed)
        }
    }
}

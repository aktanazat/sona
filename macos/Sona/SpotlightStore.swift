import AppIntents
import CoreSpotlight

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
    /// What the index holds, by id. Nil until the first write since launch,
    /// which replaces whatever an earlier run left.
    private var people: [String: SystemPerson]?
    private var meetings: [MeetingSessionId: SystemMeeting]?
    private var tasks: [String: SystemTask]?
    private var projects: [String: SystemProject]?
    private var notes: [String: SystemNote]?
    private var syncing = false
    /// What changed since the last read. A meeting change marks the work too:
    /// keeping a series on this Mac hides the tasks drawn from it.
    private var due: Set<Part> = []
    private var pending: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        for name in [CoreEvent.meetingArtifactChanged, CoreEvent.meetingSessionChanged, CoreEvent.meetingRemoved] {
            core.observe(name) { [weak self] _ in self?.refresh() }
        }
        for name in ["work-changed", "planning-changed-event", CoreEvent.scratchpadChanged] {
            core.observe(name) { [weak self] _ in self?.refresh([.work]) }
        }
    }

    func start() async {
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
        while !due.isEmpty {
            let parts = due
            due = []
            if parts.contains(.meetings) { await reindexMeetings() }
            if parts.contains(.work) { await reindexWork() }
        }
        syncing = false
    }

    private func reindexMeetings() async {
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

    private func reindexWork() async {
        guard let list: SystemWork = try? await core.request("sona_query_system_work") else { return }
        let tasks = Dictionary(list.tasks.map { ($0.id, $0) }) { first, _ in first }
        let projects = Dictionary(list.projects.map { ($0.id, $0) }) { first, _ in first }
        let notes = Dictionary(list.notes.map { ($0.id, $0) }) { first, _ in first }
        do {
            try await write(tasks, over: self.tasks, as: TaskEntity.init)
            try await write(projects, over: self.projects, as: ProjectEntity.init)
            try await write(notes, over: self.notes, as: NoteEntity.init)
        } catch {
            self.tasks = nil
            self.projects = nil
            self.notes = nil
            return
        }
        self.tasks = tasks
        self.projects = projects
        self.notes = notes
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

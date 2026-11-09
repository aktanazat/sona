import Foundation
import Observation

enum PersonWorkIdentityStatus: String, Decodable {
    case available, merged, unavailable
}

struct PersonWorkProject: Decodable, Identifiable {
    let id: String
    let title: String
    let status: PlanningProjectStatus
    let revision: Int64
}

enum PersonWorkRelation: String, Decodable {
    case explicitSelection = "explicit_selection"
    case confirmedConversation = "confirmed_conversation"
}

struct PersonWorkConversation: Decodable, Identifiable {
    let id: String
    let project: PersonWorkProject
    let targetKind: PlanningTargetKind
    let targetId: String
    let targetTitle: String
    let meetingId: String
    let meetingTitle: String
    let meetingStartedAtMs: Int64?
    let atMs: Int64?
    let revision: Int64
    let relation: PersonWorkRelation

    var timestamp: String? {
        guard let atMs else { return nil }
        let seconds = atMs / 1000
        return String(format: "%lld:%02lld", seconds / 60, seconds % 60)
    }
}

struct PersonWorkSnapshot: Decodable {
    let personId: String
    let canonicalPersonId: String?
    let identityStatus: PersonWorkIdentityStatus
    let conversations: [PersonWorkConversation]
}

@MainActor
@Observable
final class PersonWorkStore {
    private(set) var snapshot: PersonWorkSnapshot?
    /// Every task they own, in triage, in a project or in none, read the way
    /// the task lists read them.
    private(set) var tasks: [WorkItemRow] = []
    private(set) var loading = false
    private(set) var error: String?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var personId: String?
    @ObservationIgnored private var generation: UInt64 = 0
    @ObservationIgnored private var active = false
    @ObservationIgnored private var refreshTask: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        for event in ["work-changed", "planning-changed-event", CoreEvent.peopleArtifactChanged,
                      CoreEvent.peopleMeetingRemoved, CoreEvent.meetingTranscriptChanged,
                      CoreEvent.meetingSessionChanged, CoreEvent.meetingNoteChanged] {
            core.observe(event) { [weak self] _ in self?.refresh() }
        }
    }

    /// Reads one person's work. The last answer stays up while a re-read of
    /// the same person is out, so the page never blanks to redraw itself.
    func load(_ personId: String) async {
        guard !Task.isCancelled else { return }
        active = true
        if self.personId != personId {
            snapshot = nil
            tasks = []
            error = nil
        }
        self.personId = personId
        refreshTask?.cancel()
        generation += 1
        let stamp = generation
        loading = true
        await read(personId, stamp: stamp)
    }

    func refresh() {
        guard active, let personId else { return }
        refreshTask?.cancel()
        generation += 1
        let stamp = generation
        loading = true
        refreshTask = Task { [weak self] in
            guard let self, self.active, self.generation == stamp, !Task.isCancelled else { return }
            await self.read(personId, stamp: stamp)
        }
    }

    func stop() {
        active = false
        generation += 1
        refreshTask?.cancel()
        refreshTask = nil
        snapshot = nil
        tasks = []
        error = nil
        loading = false
    }

    private func read(_ personId: String, stamp: UInt64) async {
        defer { if generation == stamp { loading = false } }
        do {
            async let work: PersonWorkSnapshot = core.request(
                "planning_person_work", PersonWorkParams(personId: personId))
            async let owned: WorkList = core.request(
                "work_list", PersonTasksParams(query: WorkQuery(ownerPersonIds: [personId])))
            let (result, list) = try await (work, owned)
            guard active, generation == stamp, !Task.isCancelled else { return }
            snapshot = result
            tasks = list.items
            error = nil
        } catch {
            guard active, generation == stamp, !Task.isCancelled else { return }
            self.error = PeopleStore.message(error)
        }
    }
}

private struct PersonWorkParams: Encodable {
    let personId: String
}

private struct PersonTasksParams: Encodable {
    let query: WorkQuery
}

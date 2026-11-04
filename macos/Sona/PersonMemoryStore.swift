import Foundation
import Observation

@MainActor
@Observable
final class PersonMemoryStore {
    private(set) var snapshot: PersonMemorySnapshot?
    private(set) var timeline: [PersonHistoryEntry] = []
    private(set) var loading = false
    private(set) var error: String?
    private(set) var visibleCount = 40
    var filter: PersonHistoryFilter = .all {
        didSet { visibleCount = 40; filterTimeline() }
    }
    private(set) var filteredTimeline: [PersonHistoryEntry] = []
    private(set) var since: Date?

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var personId: String?
    @ObservationIgnored private var generation: UInt64 = 0
    @ObservationIgnored private var active = false
    @ObservationIgnored private var refreshPending = false

    init(core: Core) {
        self.core = core
        for event in [CoreEvent.peopleArtifactChanged, CoreEvent.peopleMeetingRemoved,
                      CoreEvent.meetingTranscriptChanged, CoreEvent.meetingSessionChanged,
                      CoreEvent.meetingNoteChanged, CoreEvent.scratchpadChanged] {
            core.observe(event) { [weak self] _ in
                self?.refresh()
            }
        }
    }

    func load(_ personId: String, since: Date? = nil) async {
        guard !Task.isCancelled else { return }
        active = true
        generation += 1
        let stamp = generation
        if self.personId != personId {
            snapshot = nil
            timeline = []
            filteredTimeline = []
            visibleCount = 40
            self.since = nil
            refreshPending = false
        }
        self.personId = personId
        if let since { self.since = since }
        loading = true
        do {
            let params = PersonMemoryParams(personId: personId,
                sinceUtcMs: self.since.map { Int64($0.timeIntervalSince1970 * 1000) })
            let snapshot: PersonMemorySnapshot = try await core.request("person_memory", params)
            guard stamp == generation, !Task.isCancelled else { return }
            self.snapshot = snapshot
            timeline = Self.history(snapshot)
            filterTimeline()
            error = nil
        } catch {
            guard stamp == generation, !Task.isCancelled else { return }
            snapshot = nil
            timeline = []
            filteredTimeline = []
            self.error = PeopleStore.message(error)
        }
        guard stamp == generation else { return }
        loading = false
        if refreshPending {
            refreshPending = false
            refresh()
        }
    }

    func refresh() {
        guard active, let personId else { return }
        if loading {
            refreshPending = true
            return
        }
        loading = true
        let stamp = generation
        Task { [weak self] in
            guard let self, self.active, self.generation == stamp else { return }
            await self.load(personId)
        }
    }

    func compare(since: Date) {
        self.since = since
        refresh()
    }

    func stop() {
        active = false
        generation += 1
        loading = false
        refreshPending = false
    }

    func showMore() { visibleCount += 40 }

    private func filterTimeline() {
        filteredTimeline = filter == .all ? timeline : timeline.filter { $0.filter == filter }
    }

    private static func history(_ snapshot: PersonMemorySnapshot) -> [PersonHistoryEntry] {
        let rows = snapshot.conversations.map(PersonHistoryEntry.conversation)
            + snapshot.contributions.map(PersonHistoryEntry.contribution)
            + snapshot.notes.map(PersonHistoryEntry.note)
            + snapshot.commitments.map(PersonHistoryEntry.commitment)
        return rows.sorted { left, right in
            let leftTime = left.source?.atUtcMs ?? 0
            let rightTime = right.source?.atUtcMs ?? 0
            if leftTime != rightTime { return leftTime > rightTime }
            let leftOffset = left.source?.startOffsetNs ?? 0
            let rightOffset = right.source?.startOffsetNs ?? 0
            if leftOffset != rightOffset { return leftOffset > rightOffset }
            return left.id < right.id
        }
    }
}

private struct PersonMemoryParams: Encodable {
    let personId: String
    let sinceUtcMs: Int64?
}

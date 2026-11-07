import AppKit
import Foundation

/// Home's end of the day: today in at most five lines, written on this Mac,
/// and the recordings Sona titled itself with where they may belong. Nothing
/// is filed until the reader taps.
@MainActor
@Observable
final class EveningStore {
    /// Absent until the first answer.
    private(set) var evening: HomeEvening?
    private(set) var writing = false
    /// The suggestions whose answer is still in flight, by id.
    private(set) var deciding: Set<String> = []
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?

    @ObservationIgnored private let core: Core
    /// A read that lands after a newer one started is stale.
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var refresh: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        core.observe(EveningEvent.changed) { [weak self] _ in
            self?.refreshSoon()
        }
        core.observe(CoreEvent.historyUpdate) { [weak self] line in
            guard let self, let update: HistoryUpdate = try? Core.payload(line) else { return }
            if case .toggled = update { return }
            self.refreshSoon()
        }
        for event in [
            CoreEvent.meetingSessionChanged, CoreEvent.overviewMeetingArtifactChanged,
            CoreEvent.overviewMeetingRemoved,
        ] {
            core.observe(event) { [weak self] _ in self?.refreshSoon() }
        }
        // A new day starts while the app is in the back.
        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: nil
        ) { [weak self] _ in
            Task { await self?.refreshSoon() }
        }
    }

    func start() async {
        await load()
    }

    /// Sums up today now and looks again at recordings not yet filed.
    func write() {
        guard !writing else { return }
        writing = true
        Task {
            await apply { try await self.core.request("home_evening_write") }
            writing = false
        }
    }

    func decide(_ suggestion: FilingSuggestion, accept: Bool) {
        guard !deciding.contains(suggestion.id) else { return }
        deciding.insert(suggestion.id)
        Task {
            await apply {
                try await self.core.request(
                    "filing_decide", FilingDecideParams(suggestionId: suggestion.id, accept: accept))
            }
            deciding.remove(suggestion.id)
        }
    }

    private func refreshSoon() {
        refresh?.cancel()
        refresh = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await self?.load()
        }
    }

    private func load() async {
        await apply { try await self.core.request("home_evening") }
    }

    private func apply(_ read: () async throws -> HomeEvening) async {
        generation += 1
        let current = generation
        do {
            let answer = try await read()
            guard current == generation else { return }
            evening = answer
            error = nil
        } catch let failure {
            guard current == generation else { return }
            error = failure.localizedDescription
        }
    }
}

enum EveningEvent {
    static let changed = "sona:evening-changed"
}

private struct FilingDecideParams: Encodable {
    let suggestionId: String
    let accept: Bool
}

struct HomeEvening: Decodable, Equatable {
    struct Lines: Decodable, Equatable {
        let lines: [String]
        let writtenAtUtcMs: Int64
        let modelId: String
    }

    let localDay: String
    let lines: Lines?
    /// Today has a meeting or a dictation the lines could cover.
    let hasActivity: Bool
    let unfiled: [UnfiledRecording]
}

struct UnfiledRecording: Decodable, Equatable, Identifiable {
    let sessionId: MeetingSessionId
    let title: String
    let startedAtUtcMs: Int64
    let endedAtUtcMs: Int64?
    let suggestions: [FilingSuggestion]

    var id: MeetingSessionId { sessionId }
}

struct FilingSuggestion: Decodable, Equatable, Identifiable {
    let id: String
    let target: FilingTarget
    let reasons: [FilingReason]
}

enum FilingTarget: Decodable, Equatable {
    case event(title: String, startUtcMs: Int64, endUtcMs: Int64)
    case person(personId: String, name: String)
    case project(projectId: String, title: String)

    private enum CodingKeys: String, CodingKey {
        case kind, title, startUtcMs, endUtcMs, personId, name, projectId
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "event":
            self = .event(
                title: try container.decode(String.self, forKey: .title),
                startUtcMs: try container.decode(Int64.self, forKey: .startUtcMs),
                endUtcMs: try container.decode(Int64.self, forKey: .endUtcMs))
        case "person":
            self = .person(
                personId: try container.decode(String.self, forKey: .personId),
                name: try container.decode(String.self, forKey: .name))
        case "project":
            self = .project(
                projectId: try container.decode(String.self, forKey: .projectId),
                title: try container.decode(String.self, forKey: .title))
        case let kind:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown filing target \(kind)")
        }
    }
}

enum FilingReason: Decodable, Equatable {
    case onCalendar(overlaps: Bool)
    case voiceMatches(name: String)
    case said(phrase: String, count: Int, segmentId: TranscriptSegmentId)

    private enum CodingKeys: String, CodingKey {
        case kind, overlaps, name, phrase, count, segmentId
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "on_calendar":
            self = .onCalendar(overlaps: try container.decode(Bool.self, forKey: .overlaps))
        case "voice_matches":
            self = .voiceMatches(name: try container.decode(String.self, forKey: .name))
        case "said":
            self = .said(
                phrase: try container.decode(String.self, forKey: .phrase),
                count: try container.decode(Int.self, forKey: .count),
                segmentId: try container.decode(TranscriptSegmentId.self, forKey: .segmentId))
        case let kind:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown filing reason \(kind)")
        }
    }
}

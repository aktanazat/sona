import AppIntents
import CoreSpotlight
import SwiftUI

// MARK: - What the core lists to the system

/// The people and meetings Siri, Shortcuts and Spotlight may name. The core
/// leaves out every meeting of a series kept on this Mac.
struct SystemIndex: Decodable {
    let people: [SystemPerson]
    let meetings: [SystemMeeting]
}

struct SystemPerson: Decodable, Equatable {
    let id: String
    let displayName: String
    let aliases: [String]
    let organization: String?

    func answers(to name: String) -> Bool {
        ([displayName] + aliases).contains { $0.localizedStandardContains(name) }
    }
}

struct SystemMeeting: Decodable, Equatable {
    let id: MeetingSessionId
    let title: String
    let createdAtUtcMs: Int64
}

/// What one person promised, newest meeting first.
struct PersonPromises: Decodable {
    let personId: String
    let displayName: String
    let promises: [PersonCommitment]
}

enum SonaIntentError: Error, CustomLocalizedStringResourceConvertible {
    case unavailable
    case personGone

    var localizedStringResource: LocalizedStringResource {
        switch self {
        case .unavailable: "Sona couldn't read your meetings. Open Sona and try again."
        case .personGone: "This person is no longer in Sona."
        }
    }
}

extension AppModel {
    func systemIndex() async throws -> SystemIndex {
        try await startedCore().request("sona_query_system_index")
    }

    func personPromises(_ personId: String) async throws -> PersonPromises? {
        try await startedCore().request("sona_query_person_promises", ["personId": personId])
    }

    func openFromIntent(_ link: String) async throws {
        _ = try await startedCore()
        open(link: link)
    }

    /// Siri and Spotlight launch Sona to run an intent, so one can arrive
    /// while the core is still starting.
    private func startedCore() async throws -> Core {
        let deadline = ContinuousClock.now + .seconds(15)
        while !ready || coreStopped {
            guard ready || coreError == nil, ContinuousClock.now < deadline else {
                throw SonaIntentError.unavailable
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        return core
    }
}

// MARK: - People and meetings

/// A person on the People page, the same one Siri, Shortcuts and Spotlight
/// name: one entry each, under the name the page shows.
struct PersonEntity: IndexedEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Person"
    static let defaultQuery = PersonQuery()

    let id: String
    let name: String
    let aliases: [String]
    let organization: String?

    init(_ person: SystemPerson) {
        id = person.id
        name = person.displayName
        aliases = person.aliases
        organization = person.organization
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(name)", subtitle: organization.map { "\($0)" })
    }

    var attributeSet: CSSearchableItemAttributeSet {
        let attributes = defaultAttributeSet
        attributes.alternateNames = aliases
        return attributes
    }
}

struct PersonQuery: EntityStringQuery {
    @Dependency private var model: AppModel

    func entities(for identifiers: [PersonEntity.ID]) async throws -> [PersonEntity] {
        try await model.systemIndex().people.filter { identifiers.contains($0.id) }.map(PersonEntity.init)
    }

    func entities(matching string: String) async throws -> [PersonEntity] {
        try await model.systemIndex().people.filter { $0.answers(to: string) }.map(PersonEntity.init)
    }

    func suggestedEntities() async throws -> [PersonEntity] {
        try await model.systemIndex().people.map(PersonEntity.init)
    }
}

struct MeetingEntity: IndexedEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Meeting"
    static let defaultQuery = MeetingQuery()

    let id: MeetingSessionId
    let title: String
    let date: Date

    init(_ meeting: SystemMeeting) {
        id = meeting.id
        title = meeting.title
        date = PeopleFormat.date(meeting.createdAtUtcMs)
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(title)", subtitle: "\(date.formatted(date: .abbreviated, time: .omitted))")
    }

    var attributeSet: CSSearchableItemAttributeSet {
        let attributes = defaultAttributeSet
        attributes.contentCreationDate = date
        return attributes
    }
}

struct MeetingQuery: EntityStringQuery {
    @Dependency private var model: AppModel

    func entities(for identifiers: [MeetingEntity.ID]) async throws -> [MeetingEntity] {
        try await model.systemIndex().meetings.filter { identifiers.contains($0.id) }.map(MeetingEntity.init)
    }

    func entities(matching string: String) async throws -> [MeetingEntity] {
        try await model.systemIndex().meetings.filter { $0.title.localizedStandardContains(string) }.map(MeetingEntity.init)
    }

    /// The newest meetings; older ones are found by title.
    func suggestedEntities() async throws -> [MeetingEntity] {
        try await model.systemIndex().meetings.prefix(50).map(MeetingEntity.init)
    }
}

struct OpenPersonIntent: OpenIntent {
    static let title: LocalizedStringResource = "Open Person"

    @Parameter(title: "Person") var target: PersonEntity
    @Dependency private var model: AppModel

    @MainActor
    func perform() async throws -> some IntentResult {
        try await model.openFromIntent("sona://person/\(target.id)")
        return .result()
    }
}

struct OpenMeetingIntent: OpenIntent {
    static let title: LocalizedStringResource = "Open Meeting"

    @Parameter(title: "Meeting") var target: MeetingEntity
    @Dependency private var model: AppModel

    @MainActor
    func perform() async throws -> some IntentResult {
        try await model.openFromIntent("sona://meeting/\(target.id)")
        return .result()
    }
}

// MARK: - Promises

enum PromisePeriod: String, AppEnum {
    case today
    case thisWeek
    case lastWeek
    case thisMonth
    case lastMonth

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Period"
    static let caseDisplayRepresentations: [PromisePeriod: DisplayRepresentation] = [
        .today: "today",
        .thisWeek: "this week",
        .lastWeek: "last week",
        .thisMonth: "this month",
        .lastMonth: "last month",
    ]

    func interval(around now: Date, in calendar: Calendar = .current) -> DateInterval? {
        switch self {
        case .today: calendar.dateInterval(of: .day, for: now)
        case .thisWeek: calendar.dateInterval(of: .weekOfYear, for: now)
        case .lastWeek: calendar.date(byAdding: .weekOfYear, value: -1, to: now)
            .flatMap { calendar.dateInterval(of: .weekOfYear, for: $0) }
        case .thisMonth: calendar.dateInterval(of: .month, for: now)
        case .lastMonth: calendar.date(byAdding: .month, value: -1, to: now)
            .flatMap { calendar.dateInterval(of: .month, for: $0) }
        }
    }
}

/// One promise as Shortcuts passes it on.
struct PromiseEntity: TransientAppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Promise"

    @Property(title: "Words") var words: String
    @Property(title: "Meeting") var meeting: String
    @Property(title: "Date") var date: Date
    @Property(title: "Link") var link: URL?

    init() {}

    init(_ promise: PersonCommitment) {
        words = promise.said
        meeting = promise.title
        date = promise.at
        link = URL(string: promise.moment)
    }

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(words)", subtitle: "\(meeting), \(date.short)")
    }
}

extension PersonCommitment {
    /// The words as they were said, when the transcript held them.
    var said: String { quote ?? text }

    var moment: String { "sona://meeting/\(meetingId)?atMs=\(atMs)" }
}

struct PersonPromisesIntent: AppIntent {
    static let title: LocalizedStringResource = "Find Promises"
    static let description: IntentDescription? = IntentDescription(
        "What someone promised you in your meetings, newest first, with the moment they said it.")

    @Parameter(title: "Person") var person: PersonEntity
    @Parameter(title: "When") var period: PromisePeriod?
    @Dependency private var model: AppModel

    static var parameterSummary: some ParameterSummary {
        Summary("Find what \(\.$person) promised \(\.$period)")
    }

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<[PromiseEntity]> & ProvidesDialog & ShowsSnippetView {
        guard let found = try await model.personPromises(person.id) else { throw SonaIntentError.personGone }
        let window = period?.interval(around: .now)
        let promises = found.promises.filter { window?.contains($0.at) ?? true }
        return .result(
            value: promises.map(PromiseEntity.init),
            dialog: IntentDialog("\(Self.answer(found.displayName, promises, period))"),
            view: PromisesSnippet(promises: Array(promises.prefix(5))))
    }

    static func answer(_ name: String, _ promises: [PersonCommitment], _ period: PromisePeriod?) -> String {
        let when = period.map { " " + String(localized: $0.localizedStringResource) } ?? ""
        guard let latest = promises.first else { return "No promises from \(name)\(when)." }
        let said = "“\(latest.said)”, in \(latest.title) on \(latest.at.short)."
        if promises.count == 1 { return "\(name) promised \(said)" }
        return "\(name) made \(promises.count) promises\(when). The latest: \(said)"
    }
}

/// A promise's moment on its meeting page, from a row under Siri's answer.
struct OpenPromiseIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Promise"
    static let isDiscoverable = false
    static let openAppWhenRun = true

    @Parameter(title: "Link") var link: String
    @Dependency private var model: AppModel

    init() {}

    init(_ promise: PersonCommitment) {
        link = promise.moment
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        try await model.openFromIntent(link)
        return .result()
    }
}

struct PromisesSnippet: View {
    let promises: [PersonCommitment]

    var body: some View {
        if !promises.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(promises) { promise in
                    Button(intent: OpenPromiseIntent(promise)) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(promise.said)
                                .lineLimit(3)
                            Text([promise.title, promise.at.short, promise.status.contradiction].compactMap { $0 }
                                .joined(separator: " · "))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding()
        }
    }
}

struct SonaShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: PersonPromisesIntent(),
            phrases: [
                "What did \(\.$person) promise me in \(.applicationName)",
                "What did \(\.$person) promise in \(.applicationName)",
                "Find promises in \(.applicationName)",
            ],
            shortTitle: "Promises",
            systemImageName: "text.quote")
        AppShortcut(
            intent: OpenPersonIntent(),
            phrases: [
                "Open \(\.$target) in \(.applicationName)",
                "Show \(\.$target) in \(.applicationName)",
            ],
            shortTitle: "Open Person",
            systemImageName: "person.crop.circle")
    }
}

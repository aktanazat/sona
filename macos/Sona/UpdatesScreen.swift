import Foundation
import Observation
import SwiftUI

/// What the updates feed shows; an empty list narrows nothing.
struct WorkFeedFilter: Codable, Equatable {
    var projectIds: [String] = []
    var initiativeIds: [String] = []
    var labelIds: [String] = []
    var healths: [PlanningProjectHealth] = []
    var since: WorkFeedSince? = .lastDays(7)

    /// Whether a project, initiative, label or health narrows it.
    var narrows: Bool {
        !projectIds.isEmpty || !initiativeIds.isEmpty || !labelIds.isEmpty || !healths.isEmpty
    }
}

/// Where the feed starts: a fixed moment, or so many days back from each
/// time it is read. None reaches back to the first update.
enum WorkFeedSince: Codable, Hashable {
    case at(Int64)
    case lastDays(Int)

    /// The starts the Since menu offers.
    static let choices: [WorkFeedSince?] = [.lastDays(7), .lastDays(30), nil]

    static func title(_ since: WorkFeedSince?) -> String {
        switch since {
        case let .at(atMs): "Since \(PlanningFormat.day(atMs))"
        case let .lastDays(days): days == 1 ? "Last day" : "Last \(days) days"
        case nil: "All time"
        }
    }

    /// "in the last 7 days", "since Nov 3".
    var span: String {
        switch self {
        case let .at(atMs): "since \(PlanningFormat.day(atMs))"
        case let .lastDays(days): days == 1 ? "in the last day" : "in the last \(days) days"
        }
    }

    private enum CodingKeys: String, CodingKey {
        case at, lastDays
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if let atMs = try container.decodeIfPresent(Int64.self, forKey: .at) {
            self = .at(atMs)
        } else {
            self = .lastDays(try container.decode(Int.self, forKey: .lastDays))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .at(atMs): try container.encode(atMs, forKey: .at)
        case let .lastDays(days): try container.encode(days, forKey: .lastDays)
        }
    }
}

/// A saved set of feed filters.
struct WorkFeed: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    var filter: WorkFeedFilter
    var createdAtMs: Int64
    var updatedAtMs: Int64
}

/// One line of the updates feed: a project update, or a weekly digest.
enum WorkUpdateEntry: Decodable, Identifiable, Equatable {
    case update(PlanningProjectUpdate, projectTitle: String)
    case digest(noticeId: String, createdAtMs: Int64, digest: WorkProjectDigest)

    private enum CodingKeys: String, CodingKey {
        case kind, update, projectTitle, noticeId, createdAtMs, digest
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .kind) {
        case "update":
            self = .update(
                try container.decode(PlanningProjectUpdate.self, forKey: .update),
                projectTitle: try container.decode(String.self, forKey: .projectTitle))
        case "digest":
            self = .digest(
                noticeId: try container.decode(String.self, forKey: .noticeId),
                createdAtMs: try container.decode(Int64.self, forKey: .createdAtMs),
                digest: try container.decode(WorkProjectDigest.self, forKey: .digest))
        case let kind:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "Unknown update entry kind \(kind)")
        }
    }

    var id: String {
        switch self {
        case let .update(update, _): "update-\(update.id)"
        case let .digest(noticeId, _, _): "digest-\(noticeId)"
        }
    }

    var createdAtMs: Int64 {
        switch self {
        case let .update(update, _): update.createdAtMs
        case let .digest(_, createdAtMs, _): createdAtMs
        }
    }

    var icon: String {
        switch self {
        case .update: WorkSubjectKind.project.icon
        case .digest: "chart.bar.doc.horizontal"
        }
    }
}

/// The projects' updates and weekly digests, newest first, as the filters
/// or a saved feed narrow them. Reads once Updates first shows, then again
/// whenever work or planning changes.
@MainActor
@Observable
final class UpdatesStore {
    private(set) var entries: [WorkUpdateEntry]?
    private(set) var feeds: [WorkFeed] = []
    private(set) var error: String?
    /// The saved feed showing, until a filter is changed.
    private(set) var feedId: String?
    private(set) var filter = WorkFeedFilter()

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var started = false
    @ObservationIgnored private var ticket = 0

    init(core: Core) {
        self.core = core
        for event in ["work-changed", "planning-changed-event", CoreEvent.meetingRemoved] {
            core.observe(event) { [weak self] _ in
                guard let self, self.started else { return }
                Task {
                    await self.load()
                    await self.loadFeeds()
                }
            }
        }
    }

    func start() async {
        started = true
        await loadFeeds()
        await load()
    }

    func load() async {
        ticket += 1
        let mine = ticket
        do {
            let read: [WorkUpdateEntry] = try await core.request("work_updates", UpdatesParams(filter: filter, limit: nil))
            guard mine == ticket else { return }
            entries = read.sorted { $0.createdAtMs > $1.createdAtMs }
            error = nil
        } catch {
            guard mine == ticket else { return }
            self.error = error.localizedDescription
        }
    }

    func loadFeeds() async {
        do {
            feeds = Self.sorted(try await core.request("work_feeds"))
        } catch {
            self.error = error.localizedDescription
        }
    }

    func edit(_ change: (inout WorkFeedFilter) -> Void) {
        change(&filter)
        feedId = nil
        Task { await load() }
    }

    func show(since: WorkFeedSince?) {
        guard filter.since != since else { return }
        edit { $0.since = since }
    }

    /// Shows a saved feed, or every update when nil.
    func pick(_ feed: WorkFeed?) {
        feedId = feed?.id
        filter = feed?.filter ?? WorkFeedFilter()
        Task { await load() }
    }

    /// Saves the filters showing as a new feed, and shows it.
    func saveFeed(named name: String) async -> Bool {
        do {
            let feed: WorkFeed = try await core.request("work_feed_save", FeedSaveParams(
                operationId: Self.newId(), id: nil, name: name, filter: filter))
            feeds = Self.sorted(feeds.filter { $0.id != feed.id } + [feed])
            feedId = feed.id
            error = nil
            return true
        } catch {
            self.error = error.localizedDescription
            return false
        }
    }

    func deleteFeed(_ feed: WorkFeed) async {
        do {
            let _: WorkFeed = try await core.request("work_feed_delete", FeedDeleteParams(
                operationId: Self.newId(), id: feed.id))
            feeds.removeAll { $0.id == feed.id }
            if feedId == feed.id { feedId = nil }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    private static func sorted(_ feeds: [WorkFeed]) -> [WorkFeed] {
        feeds.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    private static func newId() -> String { UUID().uuidString.lowercased() }
}

private struct UpdatesParams: Encodable {
    let filter: WorkFeedFilter
    let limit: Int?
}
private struct FeedSaveParams: Encodable {
    let operationId: String
    let id: String?
    let name: String
    let filter: WorkFeedFilter
}
private struct FeedDeleteParams: Encodable {
    let operationId: String
    let id: String
}

/// Updates: what was posted on the projects and the weekly digests, beside
/// the inbox. An update opens its project; a digest opens beside the list.
struct UpdatesScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var digest: WorkProjectDigest?
    @State private var naming = false
    @State private var feedName = ""

    var body: some View {
        let store = model.updatesFeed
        let focus = model.workFocus
        VStack(spacing: 0) {
            WorkBar {
                InboxTabs()
            } trailing: {
                feedMenu(store)
            }
            UpdatesFilterBar(store: store)
            Hairline()
            if let entries = store.entries {
                if entries.isEmpty {
                    TaskEmpty(line: store.filter.narrows ? "No updates match."
                        : store.filter.since.map { "No updates \($0.span)." } ?? "No updates yet.")
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ErrorNote(store.error)
                                .padding(.horizontal, 24)
                            ForEach(entries) { entry in
                                UpdatesRow(entry: entry) { open(entry) }
                            }
                        }
                        .padding(.vertical, 6)
                    }
                }
            } else if let error = store.error {
                TaskFailure(message: error) { Task { await store.load() } }
            } else {
                PlannerLoadingRows(label: "Loading updates")
                    .padding(24)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
        .overlay(alignment: .trailing) {
            if focus.peeking, let digest {
                ProjectDigestPanel(digest: digest)
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .animation(Motion.regular, value: focus.peeking)
        .task {
            if model.projects.initiatives.isEmpty { await model.projects.loadInitiatives() }
            await store.start()
        }
        .onDisappear { focus.peeking = false }
        .alert("Save as feed", isPresented: $naming) {
            TextField("Name", text: $feedName)
            Button("Save") {
                let name = feedName.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !name.isEmpty else { return }
                Task { _ = await store.saveFeed(named: name) }
            }
            Button("Cancel", role: .cancel) {}
        }
    }

    private func open(_ entry: WorkUpdateEntry) {
        switch entry {
        case let .update(update, _):
            model.openProject(update.projectId)
        case let .digest(_, _, digest):
            self.digest = digest
            model.workFocus.peeking = true
        }
    }

    private func feedMenu(_ store: UpdatesStore) -> some View {
        let current = store.feeds.first { $0.id == store.feedId }
        return Menu {
            Toggle("All updates", isOn: Binding(get: { store.feedId == nil }, set: { _ in store.pick(nil) }))
            ForEach(store.feeds) { feed in
                Toggle(feed.name, isOn: Binding(get: { store.feedId == feed.id }, set: { _ in store.pick(feed) }))
            }
            Divider()
            Button("Save as feed…") {
                feedName = ""
                naming = true
            }
            if let current {
                Button("Delete “\(current.name)”", role: .destructive) { Task { await store.deleteFeed(current) } }
            }
        } label: {
            Text(current?.name ?? "Feeds").lineLimit(1)
        }
        .menuStyle(.button)
        .buttonStyle(UpdatesChip(on: current != nil))
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Saved feeds")
        .accessibilityLabel(current.map { "Feed, \($0.name)" } ?? "Saved feeds")
    }
}

private struct UpdatesChoice<Value: Hashable>: Identifiable {
    let id: Value
    let name: String
}

/// Project, initiative, label, health and how far back, across the top of
/// the feed.
private struct UpdatesFilterBar: View {
    let store: UpdatesStore
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 6) {
            menu("Project", model.work.projects.map { UpdatesChoice(id: $0.id, name: $0.title) }, \.projectIds)
            menu("Initiative", model.projects.initiatives.map { UpdatesChoice(id: $0.id, name: $0.name) }, \.initiativeIds)
            menu("Label", model.work.labels.filter { $0.retiredAtMs == nil }.map { UpdatesChoice(id: $0.id, name: $0.name) },
                 \.labelIds)
            menu("Health", PlanningProjectHealth.allCases.map { UpdatesChoice(id: $0, name: $0.label) }, \.healths)
            let since = store.filter.since
            let title = WorkFeedSince.title(since)
            Menu {
                let choices = WorkFeedSince.choices.contains(since) ? WorkFeedSince.choices
                    : [since] + WorkFeedSince.choices
                ForEach(choices, id: \.self) { choice in
                    Toggle(WorkFeedSince.title(choice), isOn: Binding(get: { since == choice },
                                                                      set: { _ in store.show(since: choice) }))
                }
            } label: {
                Text(title).lineLimit(1)
            }
            .menuStyle(.button)
            .buttonStyle(UpdatesChip(on: since != .lastDays(7)))
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityLabel("Since, \(title)")
            Spacer(minLength: 8)
            if store.filter.narrows {
                Button("Clear filters") { store.edit { $0 = WorkFeedFilter(since: $0.since) } }
                    .buttonStyle(QuietButton(compact: true))
            }
        }
        .padding(.horizontal, 16)
        .frame(height: 40)
    }

    private func menu<Value: Hashable>(
        _ title: String, _ choices: [UpdatesChoice<Value>], _ path: WritableKeyPath<WorkFeedFilter, [Value]>
    ) -> some View {
        let chosen = store.filter[keyPath: path]
        let names = choices.filter { chosen.contains($0.id) }.map(\.name)
        let label = switch names.count {
        case 0: title
        case 1: names[0]
        default: "\(title): \(names.count)"
        }
        return Menu {
            if choices.isEmpty {
                Text("None yet")
            }
            ForEach(choices) { choice in
                Toggle(choice.name, isOn: Binding(get: { chosen.contains(choice.id) }, set: { on in
                    store.edit { filter in
                        filter[keyPath: path].removeAll { $0 == choice.id }
                        if on { filter[keyPath: path].append(choice.id) }
                    }
                }))
            }
        } label: {
            Text(label).lineLimit(1)
        }
        .menuStyle(.button)
        .buttonStyle(UpdatesChip(on: !chosen.isEmpty))
        .menuIndicator(.hidden)
        .fixedSize()
        .accessibilityLabel("\(title), \(names.isEmpty ? "any" : names.joined(separator: ", "))")
    }
}

/// A filter's name and a chevron, filled while it narrows the feed.
private struct UpdatesChip: ButtonStyle {
    let on: Bool
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.label
            Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold))
                .foregroundStyle(Theme.inkTertiary)
                .accessibilityHidden(true)
        }
        .fixedSize()
        .font(TypeScale.list)
        .foregroundStyle(on || hovering ? Theme.ink : Theme.inkSecondary)
        .padding(.horizontal, 10)
        .frame(height: 28)
        .background(
            configuration.isPressed ? Theme.controlPressed
                : on ? Theme.control
                : hovering ? Theme.controlHover : .clear,
            in: RoundedRectangle(cornerRadius: Theme.radiusControl))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

private struct UpdatesRow: View {
    let entry: WorkUpdateEntry
    let open: () -> Void
    @Environment(AppModel.self) private var model
    @State private var hovering = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: entry.icon).font(.system(size: 12))
                .foregroundStyle(Theme.inkSecondary)
                .frame(width: 14)
            content
            Spacer(minLength: 12)
            Text(TaskDates.ago(entry.createdAtMs, now: model.work.now)).metaText().monospacedDigit()
                .help(PlanningFormat.date(entry.createdAtMs))
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 12)
        .background(
            RoundedRectangle(cornerRadius: Theme.radiusRow)
                .fill(hovering ? Theme.wash : Color.clear)
                .padding(.horizontal, 8))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture(perform: open)
        .allowsWindowActivationEvents()
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction { open() }
    }

    @ViewBuilder
    private var content: some View {
        switch entry {
        case let .update(update, projectTitle):
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    Text(projectTitle)
                        .bodyText(13.5)
                        .fontWeight(.semibold)
                        .lineLimit(1)
                    ProjectHealthPill(health: update.health)
                }
                Text("\(Self.author(update)) · \(update.progress)% done\(update.editedAtMs != nil ? " · Edited" : "")")
                    .metaText(Theme.inkSecondary)
                if let body = update.body, !body.isEmpty {
                    Text(Self.excerpt(body))
                        .bodyText(13, Theme.inkSecondary)
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        case let .digest(_, _, digest):
            VStack(alignment: .leading, spacing: 4) {
                Text("Weekly project digest").bodyText(13.5).fontWeight(.semibold)
                Text("\(PlanningFormat.day(digest.fromMs)) – \(PlanningFormat.day(digest.toMs)) · "
                    + (digest.projects.count == 1 ? "1 project" : "\(digest.projects.count) projects"))
                    .metaText(Theme.inkSecondary)
            }
        }
    }

    private static func author(_ update: PlanningProjectUpdate) -> String {
        switch update.author.kind {
        case .me: "You"
        case .sona: "Sona"
        case .agent: update.author.name ?? "An agent"
        }
    }

    private static func excerpt(_ body: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: body, options: options)) ?? AttributedString(body)
    }
}

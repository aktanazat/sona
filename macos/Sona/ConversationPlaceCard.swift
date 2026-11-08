import MapKit
import SwiftUI

struct ConversationPlace: Decodable, Equatable {
    let name: String
    let latitude: Double
    let longitude: Double
    var coordinate: CLLocationCoordinate2D { .init(latitude: latitude, longitude: longitude) }
}

struct ConversationPlaceSession: Decodable, Identifiable {
    let sessionId: String
    let title: String
    let startedAtUtcMs: Int64?
    let location: ConversationPlace
    let people: [String]
    var id: String { sessionId }
}

struct ConversationPlaceReview: Decodable {
    let sessionId: String
    let location: ConversationPlace?
    let sessions: [ConversationPlaceSession]
}

/// `ConversationBookmarkKind`: what a bookmark marks, as the person pressing
/// it said. The notes list what was said just before a decision mark as a
/// decision, just before a to-do mark as an action item, and the question
/// asked just before a question mark as an open question, and keep what was
/// said just before a plain bookmark.
enum ConversationBookmarkKind: String, Codable, CaseIterable, Identifiable {
    case mark, decision, question, todo

    var id: Self { self }

    /// The three a recording offers beside the plain bookmark.
    static let typed: [ConversationBookmarkKind] = [.decision, .question, .todo]

    var label: String {
        switch self {
        case .mark: "Bookmark"
        case .decision: "Decision"
        case .question: "Question"
        case .todo: "To-do"
        }
    }

    var plural: String {
        switch self {
        case .mark: "Bookmarks"
        case .decision: "Decisions"
        case .question: "Questions"
        case .todo: "To-dos"
        }
    }

    var symbol: String {
        switch self {
        case .mark: "bookmark.fill"
        case .decision: "checkmark.seal.fill"
        case .question: "questionmark.circle.fill"
        case .todo: "checklist"
        }
    }

    var color: Color {
        switch self {
        case .mark: Theme.inkSecondary
        case .decision: Theme.accent
        case .question: Theme.tileHome
        case .todo: Theme.markTodo
        }
    }
}

struct ConversationBookmark: Decodable, Identifiable {
    let bookmarkId: String
    let sessionId: String
    let startOffsetNs: UInt64
    let label: String?
    let kind: ConversationBookmarkKind
    let createdAtUtcMs: Int64
    var id: String { bookmarkId }
}

struct ConversationBookmarkCreateParams: Encodable {
    struct Request: Encodable {
        let bookmarkId: String
        let sessionId: String
        let startOffsetNs: UInt64
        let label: String?
        let kind: ConversationBookmarkKind

        enum CodingKeys: String, CodingKey {
            case bookmarkId = "bookmark_id"
            case sessionId = "session_id"
            case startOffsetNs = "start_offset_ns"
            case label
            case kind
        }
    }

    let request: Request
}

struct ConversationPlaceCard: View {
    let core: Core
    let store: MeetingsStore
    let sessionId: String
    let revision: Int
    @State private var review: ConversationPlaceReview?
    @State private var failure: String?
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let location = review?.location {
                DisclosureGroup(isExpanded: $expanded) {
                    VStack(alignment: .leading, spacing: 12) {
                        Map(initialPosition: .region(MKCoordinateRegion(center: location.coordinate,
                            latitudinalMeters: 1_000, longitudinalMeters: 1_000))) {
                            Marker(location.name, coordinate: location.coordinate)
                        }
                        .mapControls { MapZoomStepper() }
                        .frame(height: 180)
                        .clipShape(RoundedRectangle(cornerRadius: Theme.radiusControl))
                        .accessibilityLabel("Map for \(location.name)")
                        if let sessions = review?.sessions, !sessions.isEmpty {
                            Text("Nearby conversations").sectionLabel()
                            ForEach(sessions) { session in
                                Button { store.open(session.sessionId) } label: {
                                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text(session.title).bodyText(14).lineLimit(2)
                                            Text(session.location.name).metaText()
                                        }
                                        Spacer(minLength: 8)
                                        if let date = session.startedAtUtcMs {
                                            Text(date.meetingDate.short).metaText()
                                        }
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(.vertical, 4)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    .padding(.top, 12)
                } label: {
                    Label(location.name, systemImage: "mappin.and.ellipse")
                        .font(TypeScale.body(13)).foregroundStyle(Theme.inkSecondary)
                }
            } else if let failure {
                HStack(spacing: 12) {
                    Text(failure).metaText(Theme.live)
                    Button("Retry") { Task { await load() } }.buttonStyle(.quiet)
                }
            }
        }
        .task(id: "\(sessionId):\(revision)") { await load() }
    }

    private func load() async {
        do {
            let result: ConversationPlaceReview = try await core.request("conversation_place_review", ["sessionId": sessionId])
            guard !Task.isCancelled, result.sessionId == sessionId else { return }
            review = result
            failure = nil
        } catch {
            guard !Task.isCancelled else { return }
            failure = "Place could not be read."
        }
    }
}

struct ConversationBookmarksView: View {
    let core: Core
    let store: MeetingsStore
    let sessionId: String
    let revision: Int
    @State private var bookmarks: [ConversationBookmark] = []
    @State private var failure: String?
    @State private var filter: ConversationBookmarkKind?

    private var kinds: [ConversationBookmarkKind] {
        ConversationBookmarkKind.allCases.filter { kind in bookmarks.contains { $0.kind == kind } }
    }

    private var shown: [ConversationBookmark] {
        guard let filter else { return bookmarks }
        return bookmarks.filter { $0.kind == filter }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !bookmarks.isEmpty {
                DisclosureGroup("Bookmarks · \(bookmarks.count)") {
                    VStack(alignment: .leading, spacing: 10) {
                        strip
                        if kinds.count > 1 {
                            HStack(spacing: 6) {
                                chip(nil, "All \(bookmarks.count)")
                                ForEach(kinds) { kind in
                                    chip(kind, "\(kind.plural) \(bookmarks.filter { $0.kind == kind }.count)")
                                }
                            }
                        }
                        LazyVStack(alignment: .leading, spacing: 8) {
                            ForEach(shown) { bookmark in
                                HStack(spacing: 12) {
                                    Button(Int64(clamping: bookmark.startOffsetNs).meetingOffsetClock) {
                                        store.playBookmark(bookmark.startOffsetNs)
                                    }
                                    .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                                    .disabled(!store.canPlayBookmark(bookmark.startOffsetNs))
                                    Image(systemName: bookmark.kind.symbol)
                                        .foregroundStyle(bookmark.kind.color)
                                        .frame(width: 16)
                                        .accessibilityHidden(true)
                                    Text(bookmark.label ?? bookmark.kind.label).bodyText(13)
                                }
                            }
                        }
                    }
                    .padding(.vertical, 10)
                }
                .font(TypeScale.body(13)).foregroundStyle(Theme.inkSecondary)
            }
            if let failure { Text(failure).metaText(Theme.live) }
        }
        .onChange(of: sessionId) { _, _ in filter = nil }
        .task(id: "\(sessionId):\(revision)") {
            do {
                let result: [ConversationBookmark] = try await core.request("conversation_bookmarks", ["sessionId": sessionId])
                guard !Task.isCancelled else { return }
                bookmarks = result
                failure = nil
            } catch {
                guard !Task.isCancelled else { return }
                failure = "Bookmarks could not be read."
            }
        }
    }

    /// Where each bookmark falls in the meeting, start to last word, in its
    /// kind's colour. Hidden when the meeting's length is unknown.
    @ViewBuilder
    private var strip: some View {
        let spoken = store.snapshot?.transcript.map(\.base.endOffsetNs).max() ?? 0
        let end = max(spoken, bookmarks.map { Int64(clamping: $0.startOffsetNs) }.max() ?? 0)
        if end > 0 {
            Canvas { context, size in
                context.fill(Path(roundedRect: CGRect(x: 0, y: size.height / 2 - 1, width: size.width, height: 2),
                                  cornerRadius: 1), with: .color(Theme.border))
                for bookmark in shown {
                    let x = size.width * CGFloat(Int64(clamping: bookmark.startOffsetNs)) / CGFloat(end)
                    let tick = CGRect(x: min(max(x - 1.5, 0), size.width - 3), y: 0, width: 3, height: size.height)
                    context.fill(Path(roundedRect: tick, cornerRadius: 1.5), with: .color(bookmark.kind.color))
                }
            }
            .frame(height: 12)
            .accessibilityHidden(true)
        }
    }

    private func chip(_ kind: ConversationBookmarkKind?, _ title: String) -> some View {
        Button(title) { filter = kind }
            .buttonStyle(QuietButton(color: filter == kind ? Theme.accent : Theme.inkSecondary, compact: true))
            .accessibilityAddTraits(filter == kind ? .isSelected : [])
    }
}

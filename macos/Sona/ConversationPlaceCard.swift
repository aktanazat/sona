import MapKit
import SwiftUI

struct ConversationPlace: Decodable {
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
    var id: String { sessionId }
}

struct ConversationPlaceReview: Decodable {
    let sessionId: String
    let location: ConversationPlace?
    let sessions: [ConversationPlaceSession]
}

struct ConversationBookmark: Decodable, Identifiable {
    let bookmarkId: String
    let sessionId: String
    let startOffsetNs: UInt64
    let label: String?
    let createdAtUtcMs: Int64
    var id: String { bookmarkId }
}

struct ConversationBookmarkCreateParams: Encodable {
    struct Request: Encodable {
        let bookmarkId: String
        let sessionId: String
        let startOffsetNs: UInt64
        let label: String?

        enum CodingKeys: String, CodingKey {
            case bookmarkId = "bookmark_id"
            case sessionId = "session_id"
            case startOffsetNs = "start_offset_ns"
            case label
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

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if !bookmarks.isEmpty {
                DisclosureGroup("Bookmarks · \(bookmarks.count)") {
                    LazyVStack(alignment: .leading, spacing: 8) {
                        ForEach(bookmarks) { bookmark in
                            HStack(spacing: 12) {
                                Button(Int64(clamping: bookmark.startOffsetNs).meetingOffsetClock) {
                                    store.playBookmark(bookmark.startOffsetNs)
                                }
                                .buttonStyle(QuietButton(color: Theme.accent, compact: true))
                                .disabled(!store.canPlayBookmark(bookmark.startOffsetNs))
                                Text(bookmark.label ?? "Bookmark").bodyText(13)
                            }
                        }
                    }
                    .padding(.top, 10)
                }
                .font(TypeScale.body(13)).foregroundStyle(Theme.inkSecondary)
            }
            if let failure { Text(failure).metaText(Theme.live) }
        }
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
}

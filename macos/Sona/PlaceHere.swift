import CoreLocation
import SwiftUI

/// The places near where a recording started, offered until he picks one,
/// turns them down or leaves the meeting. Memory only: the reading is never
/// saved, and a place he picks is saved at its own coordinate.
@MainActor
@Observable
final class PlaceReadings {
    struct Offer {
        let fromPhone: Bool
        let places: [ConversationLocation]
    }

    private(set) var offers: [MeetingSessionId: Offer] = [:]
    @ObservationIgnored private var asked: Set<MeetingSessionId> = []
    @ObservationIgnored private var waiting: Set<MeetingSessionId> = []
    @ObservationIgnored private var closed: Set<MeetingSessionId> = []
    @ObservationIgnored private let location = LocationOnce()
    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
    }

    /// A recording started on this Mac: reads where it is once, unless the
    /// recording has a place or its invite pins one. A reading that would ask
    /// for permission waits for `recordingStopped`: nothing asks during a call.
    func recordingStarted(_ sessionId: MeetingSessionId) {
        guard asked.insert(sessionId).inserted else { return }
        if location.asks {
            waiting.insert(sessionId)
        } else {
            read(for: sessionId)
        }
    }

    /// The recording stopped: takes the reading it held back.
    func recordingStopped(_ sessionId: MeetingSessionId) {
        guard waiting.remove(sessionId) != nil else { return }
        read(for: sessionId)
    }

    /// A meeting from the phone opened: offers the places near where the
    /// phone started recording it, once.
    func phoneRecordingOpened(_ sessionId: MeetingSessionId) async {
        guard asked.insert(sessionId).inserted,
              let reading: PhoneReading = try? await core.request(
                  "conversation_place_reading_take", ["sessionId": sessionId])
        else { return }
        let started = CLLocation(
            coordinate: CLLocationCoordinate2D(latitude: reading.latitude, longitude: reading.longitude),
            altitude: 0, horizontalAccuracy: reading.accuracyMeters, verticalAccuracy: -1, timestamp: .now)
        offer(await placesNear(started), to: sessionId, fromPhone: true)
    }

    func use(_ place: ConversationLocation, for sessionId: MeetingSessionId) async throws {
        try await saveConversationPlace(core: core, sessionId: sessionId, place: place)
        drop(sessionId)
    }

    /// Lets the offer go for good, with any reading still on its way.
    func drop(_ sessionId: MeetingSessionId) {
        closed.insert(sessionId)
        waiting.remove(sessionId)
        offers[sessionId] = nil
    }

    private func read(for sessionId: MeetingSessionId) {
        Task {
            guard await placeUnknown(sessionId), let reading = await location.read() else { return }
            offer(await placesNear(reading), to: sessionId, fromPhone: false)
        }
    }

    private func offer(_ places: [ConversationLocation], to sessionId: MeetingSessionId, fromPhone: Bool) {
        guard !places.isEmpty, !closed.contains(sessionId) else { return }
        offers[sessionId] = Offer(fromPhone: fromPhone, places: places)
    }

    private func placeUnknown(_ sessionId: MeetingSessionId) async -> Bool {
        guard let review: ConversationPlaceReview = try? await core.request(
                  "conversation_place_review", ["sessionId": sessionId]),
              review.location == nil,
              let suggestions: [FilingSuggestion] = try? await core.request(
                  "context_suggestions", ["sessionId": sessionId])
        else { return false }
        return !suggestions.contains { if case .place = $0.target { true } else { false } }
    }
}

private struct PhoneReading: Decodable {
    let latitude: Double
    let longitude: Double
    let accuracyMeters: Double
}

/// "Maybe at" the place nearest where the recording started, with the next
/// nearest to choose instead.
struct PlaceOfferRow: View {
    let places: PlaceReadings
    let sessionId: MeetingSessionId
    @State private var chosen: ConversationLocation?
    @State private var saving = false
    @State private var failure: String?

    var body: some View {
        let offer = places.offers[sessionId]
        VStack(alignment: .leading, spacing: 12) {
            if let offer {
                let place = chosen.flatMap { offer.places.contains($0) ? $0 : nil } ?? offer.places[0]
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("Maybe at \(place.name)").bodyText(13)
                        HStack(spacing: 6) {
                            Text(offer.fromPhone ? "near where your phone was" : "near where you were").metaText()
                            let others = offer.places.filter { $0 != place }
                            if !others.isEmpty { Text("· or").metaText() }
                            ForEach(Array(others.enumerated()), id: \.offset) { index, other in
                                if index > 0 { Text("·").metaText() }
                                Button { chosen = other } label: {
                                    Text(other.name).metaText(Theme.inkSecondary)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                    Spacer(minLength: 8)
                    Button("Not this") { places.drop(sessionId) }
                        .buttonStyle(.quiet)
                    Button("Use") { use(place) }
                        .buttonStyle(QuietButton(compact: true))
                }
                .disabled(saving)
                if let failure { Text(failure).metaText(Theme.live) }
            }
        }
        .padding(.bottom, offer == nil ? 0 : 16)
    }

    private func use(_ place: ConversationLocation) {
        saving = true
        Task {
            defer { saving = false }
            do {
                try await places.use(place, for: sessionId)
                failure = nil
            } catch {
                failure = "Place could not be saved. \(error.localizedDescription)"
            }
        }
    }
}

/// On the live page: the places near where this recording started, once it
/// stops, and once it has a place, the last meeting within 100 m of it.
struct LivePlaceLine: View {
    let places: PlaceReadings
    let sessionId: MeetingSessionId
    let revision: Int
    @Environment(AppModel.self) private var model
    @State private var before: ConversationPlaceSession?

    var body: some View {
        let recording = model.live.recording(sessionId)
        VStack(alignment: .leading, spacing: 0) {
            if !recording {
                PlaceOfferRow(places: places, sessionId: sessionId)
            }
            if let before {
                Button { model.openPast(before.sessionId, segmentId: nil) } label: {
                    Text("Here before: \(before.people.isEmpty ? before.title : before.people.joined(separator: ", "))"
                        + (before.startedAtUtcMs.map { ", \($0.meetingDate.short)" } ?? "") + " ▶")
                        .metaText(Theme.inkSecondary)
                }
                .buttonStyle(.plain)
                .padding(.bottom, 16)
            }
        }
        .task(id: "\(sessionId):\(revision):\(recording)") {
            if !recording { places.recordingStopped(sessionId) }
            await load()
        }
    }

    private func load() async {
        guard let review: ConversationPlaceReview = try? await model.core.request(
                  "conversation_place_review", ["sessionId": sessionId]),
              !Task.isCancelled, review.sessionId == sessionId
        else { return }
        if review.location != nil { places.drop(sessionId) }
        before = review.location == nil ? nil : review.sessions.first
    }
}

/// On a meeting's page: the places near where it was recorded, until he
/// picks one or leaves the page.
struct ReviewPlaceOffer: View {
    let places: PlaceReadings
    let sessionId: MeetingSessionId
    let revision: Int
    @Environment(AppModel.self) private var model

    var body: some View {
        PlaceOfferRow(places: places, sessionId: sessionId)
            .task(id: "\(sessionId):\(revision)") {
                guard let review: ConversationPlaceReview = try? await model.core.request(
                          "conversation_place_review", ["sessionId": sessionId]),
                      !Task.isCancelled
                else { return }
                if review.location == nil {
                    if !model.live.recording(sessionId) { places.recordingStopped(sessionId) }
                    await places.phoneRecordingOpened(sessionId)
                } else {
                    places.drop(sessionId)
                }
            }
            .onChange(of: sessionId) { left, _ in places.drop(left) }
            .onDisappear { places.drop(sessionId) }
    }
}

import AppKit
import MapKit
import SwiftUI

private enum ConversationKind: String, Codable, CaseIterable, Identifiable {
    case unspecified, huddle
    case onlineCall = "online_call"
    case inPerson = "in_person"
    case noteSession = "note_session"
    case imported

    var id: Self { self }
    var label: String {
        switch self {
        case .unspecified: "Conversation"
        case .huddle: "Huddle"
        case .onlineCall: "Online call"
        case .inPerson: "In person"
        case .noteSession: "Note session"
        case .imported: "Imported recording"
        }
    }
    var symbol: String {
        switch self {
        case .unspecified: "bubble.left.and.bubble.right"
        case .huddle: "headphones"
        case .onlineCall: "video"
        case .inPerson: "person.2"
        case .noteSession: "square.and.pencil"
        case .imported: "square.and.arrow.down"
        }
    }
}

private struct ConversationLocation: Codable, Equatable {
    let name: String
    let latitude: Double
    let longitude: Double

    var coordinate: CLLocationCoordinate2D {
        CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }
}

private struct ConversationContext: Decodable {
    let sessionId: String
    let revision: UInt64
    let kind: ConversationKind
    let origin: String
    let applicationBundleIds: [String]
    let capturedMicrophone: Bool
    let capturedSystemAudio: Bool
    let calendarName: String?
    let calendarLocation: String?
    let location: ConversationLocation?

    var source: String {
        var parts: [String] = []
        if !applicationBundleIds.isEmpty {
            let names = applicationBundleIds.map { InstalledApp.find($0)?.name ?? $0 }
            parts.append("Audio route: \(names.joined(separator: ", "))")
        }
        if origin == "import" {
            parts.append("Imported file")
        } else {
            if capturedMicrophone { parts.append("Microphone") }
            if capturedSystemAudio { parts.append("App audio") }
        }
        if let calendarName { parts.append(calendarName) }
        return parts.joined(separator: " · ")
    }
}

/// `MailHotelStay`: a hotel stay a booking email in Mail gives for the
/// recording's day.
private struct MailHotelStay: Decodable {
    let hotel: String
    let checkIn: String
    let checkOut: String
    let subject: String
    let sender: String
    let receivedAt: String

    private static let isoDay: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd"
        return formatter
    }()

    /// Where the guess came from, in his words: the stay and the email.
    var source: String {
        let from = sender.split(separator: "<").first.map { $0.trimmingCharacters(in: .whitespaces) } ?? ""
        let who = from.isEmpty ? sender : from
        return "Your stay \(Self.day(checkIn)) to \(Self.day(checkOut)), from the email “\(subject)” sent by \(who) on \(Self.day(String(receivedAt.prefix(10))))."
    }

    private static func day(_ text: String) -> String {
        isoDay.date(from: text)?.formatted(.dateTime.month(.abbreviated).day()) ?? text
    }
}

private struct MailHotelLookup: Decodable {
    let state: String
    let stay: MailHotelStay?
}

struct ConversationContextView: View {
    let sessionId: String
    let editable: Bool
    let partialRecording: Bool
    @Environment(AppModel.self) private var model
    @State private var context: ConversationContext?
    @State private var failure: String?
    @State private var editing = false
    @State private var hotel: MailHotelLookup?
    @State private var suggestedPlace: String?
    @AppStorage("meetings.dismissedHotelSuggestions") private var dismissedHotels = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let context {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Label {
                        Text(context.kind.label)
                    } icon: {
                        if let app = context.applicationBundleIds.first {
                            AppMark(bundleId: app, fallback: context.kind.symbol, size: 15)
                                .accessibilityHidden(true)
                        } else {
                            Image(systemName: context.kind.symbol)
                        }
                    }
                    .font(TypeScale.body(13))
                    .foregroundStyle(Theme.inkSecondary)
                    if !context.source.isEmpty {
                        Text(context.source).metaText().lineLimit(2)
                    }
                    if partialRecording && context.origin != "import" {
                        Text("Partial recording").metaText(Theme.live)
                    }
                    Spacer(minLength: 8)
                    if let location = context.calendarLocation {
                        Text(location).metaText().lineLimit(1)
                    }
                    if editable {
                        Button("Context…") { suggestedPlace = nil; editing = true }
                            .buttonStyle(QuietButton(compact: true))
                    }
                }
                if editable, context.location == nil, !hotelDismissed, let stay = hotel?.stay {
                    hotelSuggestion(stay)
                }
            } else if let failure {
                HStack {
                    Text(failure).metaText(Theme.live)
                    Button("Try again") { Task { await load() } }.buttonStyle(.quiet)
                }
            }
        }
        .padding(.bottom, context == nil ? 0 : 16)
        .task(id: sessionId) { await load() }
        .sheet(isPresented: $editing) {
            if let context {
                ConversationContextEditor(
                    context: context, core: model.core, suggestedPlace: suggestedPlace,
                    mailClosed: hotel?.state == "closed"
                ) { saved in
                    self.context = saved
                    editing = false
                }
            }
        }
    }

    private func load() async {
        context = nil
        failure = nil
        hotel = nil
        do {
            let result: ConversationContext = try await model.core.request(
                "conversation_context", ["sessionId": sessionId])
            guard !Task.isCancelled else { return }
            context = result
            if editable, result.location == nil, result.calendarLocation == nil, !hotelDismissed {
                hotel = try? await placeFromMail(core: model.core, sessionId: sessionId, openMail: false)
            }
        } catch {
            guard !Task.isCancelled else { return }
            failure = "Conversation context could not be read. \(error.localizedDescription)"
        }
    }

    private var hotelDismissed: Bool { dismissedHotels.split(separator: " ").contains { $0 == sessionId } }

    /// A maybe, never a write: he picks the place in Maps and saves it.
    private func hotelSuggestion(_ stay: MailHotelStay) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Label("Maybe at \(stay.hotel)", systemImage: "bed.double").bodyText(13)
                Text(stay.source).metaText().fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            Button("Not this") { dismissedHotels += " \(sessionId)" }
                .buttonStyle(.quiet)
            Button("Use this place…") {
                suggestedPlace = stay.hotel
                editing = true
            }
            .buttonStyle(QuietButton(compact: true))
        }
    }
}


private struct ConversationContextEditor: View {
    let context: ConversationContext
    let core: Core
    let mailClosed: Bool
    let saved: (ConversationContext) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var kind: ConversationKind
    @State private var location: ConversationLocation?
    @State private var query: String
    @State private var places: [MKMapItem] = []
    @State private var searching = false
    @State private var saving = false
    @State private var failure: String?
    @State private var mailStay: MailHotelStay?
    @State private var lookingInMail = false

    init(context: ConversationContext, core: Core, suggestedPlace: String?, mailClosed: Bool,
         saved: @escaping (ConversationContext) -> Void) {
        self.context = context
        self.core = core
        self.mailClosed = mailClosed
        self.saved = saved
        _kind = State(initialValue: context.kind)
        _location = State(initialValue: context.location)
        _query = State(initialValue: context.location?.name ?? suggestedPlace ?? context.calendarLocation ?? "")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Text("Conversation context").titleText()
            Picker("Kind", selection: $kind) {
                ForEach(ConversationKind.allCases) { kind in Text(kind.label).tag(kind) }
            }
            VStack(alignment: .leading, spacing: 10) {
                Text("Location").sectionLabel()
                HStack {
                    InputField(prompt: "Place or address", text: $query)
                        .onSubmit { Task { await search() } }
                    Button(searching ? "Searching…" : "Search Maps") {
                        Task { await search() }
                    }
                    .buttonStyle(.compact)
                    .disabled(searching || query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                if !places.isEmpty {
                    ForEach(Array(places.enumerated()), id: \.offset) { _, place in
                        Button {
                            let coordinate: CLLocationCoordinate2D
                            if #available(macOS 26, *) {
                                coordinate = place.location.coordinate
                            } else {
                                coordinate = place.placemark.coordinate
                            }
                            location = ConversationLocation(
                                name: place.name ?? query,
                                latitude: coordinate.latitude, longitude: coordinate.longitude)
                            query = location?.name ?? query
                            places = []
                        } label: {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(place.name ?? "Place").bodyText(14)
                                if #available(macOS 26, *) {
                                    Text(place.addressRepresentations?.fullAddress(
                                        includingRegion: true, singleLine: true) ?? "").metaText()
                                } else {
                                    Text(place.placemark.title ?? "").metaText()
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .buttonStyle(.plain)
                        .padding(.vertical, 4)
                    }
                }
                if let location {
                    HStack {
                        Label(location.name, systemImage: "mappin").bodyText(13)
                        Spacer()
                        Button("Remove") { self.location = nil; query = ""; places = [] }
                            .buttonStyle(.quiet)
                    }
                }
                if let mailStay {
                    Text("Maybe \(mailStay.hotel). \(mailStay.source)")
                        .metaText().fixedSize(horizontal: false, vertical: true)
                } else if mailClosed && location == nil {
                    Button(lookingInMail ? "Looking in Mail…" : "Look in Mail for a hotel booking") {
                        Task { await lookInMail() }
                    }
                    .buttonStyle(.quiet)
                    .disabled(lookingInMail)
                }
                Text("Only a place you choose is saved. Searching sends the place name to Apple Maps, not your conversation.")
                    .metaText().fixedSize(horizontal: false, vertical: true)
            }
            if let failure { Text(failure).bodyText(13, Theme.live) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.buttonStyle(.secondary)
                Button(saving ? "Saving…" : "Save") { Task { await save() } }
                    .buttonStyle(.primary).disabled(saving || searching)
            }
        }
        .padding(24)
        .frame(width: 500)
        .background(Theme.page)
    }

    private func search() async {
        searching = true
        failure = nil
        defer { searching = false }
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = query
        do {
            let response = try await MKLocalSearch(request: request).start()
            places = Array(response.mapItems.prefix(5))
            if places.isEmpty { failure = "No places matched. Try a more specific address." }
        } catch {
            failure = "Maps could not find that place. \(error.localizedDescription)"
        }
    }

    /// Opens Mail if it is closed, on his tap only, and fills the search
    /// with the hotel a booking email gives for this day.
    private func lookInMail() async {
        lookingInMail = true
        failure = nil
        defer { lookingInMail = false }
        do {
            let result = try await placeFromMail(core: core, sessionId: context.sessionId, openMail: true)
            if let stay = result.stay {
                mailStay = stay
                query = stay.hotel
            } else {
                failure = result.state == "permission_denied"
                    ? "Allow Sona to control Mail in System Settings → Privacy & Security → Automation."
                    : "No hotel booking in Mail covers this day."
            }
        } catch {
            failure = "Mail could not be read. \(error.localizedDescription)"
        }
    }

    private func save() async {
        saving = true
        failure = nil
        defer { saving = false }
        do {
            let request = ContextUpdate(
                sessionId: context.sessionId, expectedRevision: context.revision,
                kind: kind, location: location)
            let result: ConversationContext = try await core.request(
                "conversation_context_update", ["request": try JSONValue(request)])
            saved(result)
        } catch {
            failure = "Context could not be saved. \(error.localizedDescription)"
        }
    }
}

private func placeFromMail(core: Core, sessionId: String, openMail: Bool) async throws -> MailHotelLookup {
    try await core.request(
        "conversation_place_from_mail", ["sessionId": JSONValue.string(sessionId), "openMail": .bool(openMail)])
}

private struct ContextUpdate: Encodable {
    let sessionId: String
    let expectedRevision: UInt64
    let kind: ConversationKind
    let location: ConversationLocation?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case expectedRevision = "expected_revision"
        case kind, location
    }
}

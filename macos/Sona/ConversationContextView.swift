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
            let names = applicationBundleIds.map { id in
                guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: id),
                      let bundle = Bundle(url: url) else { return id }
                return bundle.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String
                    ?? bundle.object(forInfoDictionaryKey: "CFBundleName") as? String ?? id
            }
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

struct ConversationContextView: View {
    let sessionId: String
    let editable: Bool
    let partialRecording: Bool
    @Environment(AppModel.self) private var model
    @State private var context: ConversationContext?
    @State private var failure: String?
    @State private var editing = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let context {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Label(context.kind.label, systemImage: context.kind.symbol)
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
                        Button("Context…") { editing = true }
                            .buttonStyle(QuietButton(compact: true))
                    }
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
                ConversationContextEditor(context: context, core: model.core) { saved in
                    self.context = saved
                    editing = false
                }
            }
        }
    }

    private func load() async {
        context = nil
        failure = nil
        do {
            let result: ConversationContext = try await model.core.request(
                "conversation_context", ["sessionId": sessionId])
            guard !Task.isCancelled else { return }
            context = result
        } catch {
            guard !Task.isCancelled else { return }
            failure = "Conversation context could not be read. \(error.localizedDescription)"
        }
    }
}


private struct ConversationContextEditor: View {
    let context: ConversationContext
    let core: Core
    let saved: (ConversationContext) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var kind: ConversationKind
    @State private var location: ConversationLocation?
    @State private var query: String
    @State private var places: [MKMapItem] = []
    @State private var searching = false
    @State private var saving = false
    @State private var failure: String?

    init(context: ConversationContext, core: Core, saved: @escaping (ConversationContext) -> Void) {
        self.context = context
        self.core = core
        self.saved = saved
        _kind = State(initialValue: context.kind)
        _location = State(initialValue: context.location)
        _query = State(initialValue: context.location?.name ?? context.calendarLocation ?? "")
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

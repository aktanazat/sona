import SwiftUI

/// `MeetingLiveDocPoint`: one point of a meeting's doc.
struct MeetingLiveDocPoint: Decodable, Identifiable, Equatable {
    let id: String
    var text: String
    var edited: Bool
    let startOffsetNs: Int64?
}

enum MeetingVoiceIntent: String, Decodable {
    case startDoc = "start_doc"
    case board, recap, ask
}

/// A spoken ask waiting for the answer on screen to be closed.
struct SpokenAsk: Equatable {
    let kind: MeetingLiveHelpKind
    let question: String?
}

/// `MeetingVoiceAsk`: something the user said to Sona on their microphone.
struct MeetingVoiceAsk: Decodable, Equatable {
    let sequence: Int
    let intent: MeetingVoiceIntent
    let words: String
    let startOffsetNs: Int64
}

/// `MeetingLiveChange`: one change to a meeting's doc or board, by whom,
/// and how far into the call it came.
struct MeetingLiveChange: Decodable, Identifiable, Equatable {
    enum Author: String, Decodable { case sona, owner, teammate }
    enum Target: String, Decodable { case point, card }
    enum Kind: String, Decodable { case added, rewrote, edited, removed }

    let id: String
    let by: Author
    /// The name a teammate gave the link; none for Sona or the user.
    let name: String?
    let atMs: Int64
    /// Nil for a change made on the meeting's page after the call.
    let callNs: Int64?
    let target: Target
    let targetId: String
    let kind: Kind
    let text: String

    var who: String {
        switch by {
        case .sona: "Sona"
        case .owner: "You"
        case .teammate: name ?? "A teammate"
        }
    }

    /// The moment in the call, as the transcript's clock shows it.
    var when: String { callNs?.meetingOffsetClock ?? "After the call" }
}

/// `MeetingLiveDoc`: the doc a meeting builds on request, what was asked
/// of Sona out loud, and every change to the doc and the board.
struct MeetingLiveDoc: Decodable, Equatable {
    let sessionId: MeetingSessionId
    let started: Bool
    var points: [MeetingLiveDocPoint]
    let asks: [MeetingVoiceAsk]
    let modelRequests: Int
    let live: Bool
    let changes: [MeetingLiveChange]

    /// The newest change to a point or a card.
    func lastChange(to targetId: String) -> MeetingLiveChange? {
        changes.last { $0.targetId == targetId }
    }
}

/// Who last changed a point or a card, and when in the call.
struct ChangeMark: View {
    let change: MeetingLiveChange

    var body: some View {
        Text("\(change.who) · \(change.when)")
            .metaText(Theme.inkTertiary)
            .lineLimit(1)
            .help(change.by == .sona ? "" : "\(change.who) changed this, so Sona leaves it as it is.")
    }
}

private struct LiveDocEditParams: Encodable {
    let sessionId: MeetingSessionId
    let pointId: String?
    let text: String
}

/// One meeting's doc: the running one in the live window, or a finished
/// one on its page.
@MainActor
@Observable
final class LiveDocStore {
    private(set) var doc: MeetingLiveDoc?
    private(set) var failure: String?
    private(set) var watching: MeetingSessionId?
    /// The doc was asked for and its first reading has not come back yet.
    private(set) var starting = false
    /// Bumped when the user asks out loud to see the board.
    private(set) var boardRequests = 0
    /// What a spoken ask turns into, set by whoever owns the live help.
    @ObservationIgnored var onAsk: (MeetingVoiceAsk) -> Void = { _ in }

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var edits = 0
    /// The newest spoken ask already acted on. Asks heard before this
    /// screen first read the doc are not acted on again.
    @ObservationIgnored private var handledAsk: Int?

    init(core: Core) {
        self.core = core
        for event in [CoreEvent.meetingLiveDocChanged, CoreEvent.meetingLiveCardChanged] {
            core.observe(event) { [weak self] line in
                guard let self, let payload: MeetingEventPayload = try? Core.payload(line),
                      payload.sessionId == self.watching else { return }
                Task { await self.load() }
            }
        }
    }

    func watch(_ sessionId: MeetingSessionId?) {
        guard watching != sessionId else { return }
        watching = sessionId
        doc = nil
        failure = nil
        starting = false
        handledAsk = nil
        Task { await load() }
    }

    func load() async {
        guard let sessionId = watching, edits == 0 else { return }
        do {
            let result: MeetingLiveDoc = try await core.request("meeting_live_doc", ["sessionId": sessionId])
            guard watching == sessionId, edits == 0 else { return }
            adopt(result)
            starting = false
            failure = nil
        } catch {
            guard watching == sessionId else { return }
            failure = "The doc could not be read."
        }
    }

    /// Start the doc from everything said so far.
    func start() {
        guard let sessionId = watching else { return }
        starting = true
        Task {
            do {
                let result: MeetingLiveDoc = try await core.request(
                    "meeting_live_doc_start", ["sessionId": sessionId])
                guard watching == sessionId else { return }
                adopt(result)
            } catch {
                guard watching == sessionId else { return }
                starting = false
                failure = "The doc could not be started."
            }
        }
    }

    /// New words for a point, a new point (`point == nil`), or the point
    /// gone when the words are blank. Later readings leave it alone.
    func edit(_ point: MeetingLiveDocPoint?, _ text: String) {
        guard let sessionId = watching else { return }
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if let point, point.text == text { return }
        if point == nil, text.isEmpty { return }
        let before = doc
        if let point, let at = doc?.points.firstIndex(where: { $0.id == point.id }) {
            if text.isEmpty {
                doc?.points.remove(at: at)
            } else {
                doc?.points[at].text = text
                doc?.points[at].edited = true
            }
        }
        edits += 1
        Task {
            defer { edits -= 1 }
            do {
                let result: MeetingLiveDoc = try await core.request(
                    "meeting_live_doc_edit",
                    LiveDocEditParams(sessionId: sessionId, pointId: point?.id, text: text))
                if edits == 1, watching == sessionId { adopt(result) }
            } catch {
                guard watching == sessionId else { return }
                doc = before
                failure = text.unicodeScalars.count > 280
                    ? "Keep a point under 280 characters." : "That change was not saved."
            }
        }
    }

    private func adopt(_ result: MeetingLiveDoc) {
        let fresh = result.asks.filter { $0.sequence > (handledAsk ?? Int.max) }
        handledAsk = result.asks.last?.sequence ?? handledAsk ?? 0
        doc = result
        for ask in fresh {
            if ask.intent == .board { boardRequests += 1 }
            onAsk(ask)
        }
    }
}

extension AppModel {
    /// What the user said to Sona during the call, answered in the live
    /// window like a typed ask. A start or a board ask needs nothing here:
    /// the core starts the doc, and the live window shows the board.
    func voiceAsk(_ ask: MeetingVoiceAsk) {
        switch ask.intent {
        case .startDoc, .board:
            break
        case .recap:
            live.askOutLoud(.recap, question: nil)
        case .ask:
            live.askOutLoud(.ask, question: String(ask.words.prefix(500)))
        }
    }
}

extension MeetingLiveHelp {
    /// The recap as it is shared: its points, led by the stretch of the
    /// call it read when that stretch starts after the first minute.
    var sharedRecap: String {
        let points = items.map { "• \($0.text)" }.joined(separator: "\n")
        guard let from = fromOffsetNs, from > 60_000_000_000, let through = throughOffsetNs else {
            return points
        }
        return "Recap of \(from.meetingOffsetClock) to \(through.meetingOffsetClock) of the call.\n\n\(points)"
    }
}

/// One point, edited in place. The words are kept when the field loses
/// focus or return is pressed; blank words remove the point.
private struct DocPointRow: View {
    let point: MeetingLiveDocPoint
    let mark: MeetingLiveChange?
    let commit: (String) -> Void
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text("•").bodyText(14, Theme.inkSecondary).accessibilityHidden(true)
            TextField("Point", text: $draft, axis: .vertical)
                .textFieldStyle(.plain)
                .font(TypeScale.body())
                .foregroundStyle(Theme.ink)
                .focused($focused)
                .onSubmit { commit(draft) }
                .accessibilityLabel("Point")
            if let mark {
                ChangeMark(change: mark)
            } else if point.edited {
                Text("Yours").metaText(Theme.inkTertiary)
                    .help("You wrote or changed this, so Sona leaves it as it is.")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .onAppear { draft = point.text }
        .onChange(of: point.text) { if !focused { draft = point.text } }
        .onChange(of: focused) { if !focused { commit(draft) } }
    }
}

/// The points of a doc, editable, and a field that adds one.
private struct DocPoints: View {
    let store: LiveDocStore
    let doc: MeetingLiveDoc
    @State private var adding = ""

    var body: some View {
        Card {
            ForEach(doc.points) { point in
                DocPointRow(point: point, mark: doc.lastChange(to: point.id)) { store.edit(point, $0) }
                Hairline().padding(.horizontal, 16)
            }
            TextField("Add a point", text: $adding,
                      prompt: Text("Add a point").foregroundStyle(Theme.inkTertiary))
                .textFieldStyle(.plain)
                .font(TypeScale.body())
                .foregroundStyle(Theme.ink)
                .onSubmit {
                    store.edit(nil, adding)
                    adding = ""
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
                .accessibilityLabel("Add a point")
        }
    }
}

/// The doc in the live window: a button that starts it, then its points
/// as they are said.
struct LiveDocSection: View {
    let sessionId: MeetingSessionId
    @Environment(AppModel.self) private var model

    var body: some View {
        let store = model.liveDoc
        let doc = store.watching == sessionId ? store.doc : nil
        PageSection("Doc") {
            if let doc, doc.started {
                if doc.points.isEmpty {
                    HStack(spacing: 8) {
                        if store.starting { ProgressView().controlSize(.small) }
                        Text(store.starting ? "Writing up what was said so far…"
                            : "Points show here as they are said.")
                            .metaText(Theme.inkTertiary)
                    }
                }
                DocPoints(store: store, doc: doc)
            } else {
                Button("Start a doc from this call") { store.start() }
                    .buttonStyle(.secondary)
                    .disabled(store.starting)
                Text("Or say “Sona, start a doc for this plan.”").metaText(Theme.inkTertiary)
            }
            if let failure = store.failure, store.watching == sessionId {
                Text(failure).metaText(Theme.inkSecondary)
            }
        }
        .onAppear { store.watch(sessionId) }
    }
}

/// A finished meeting's doc, on the meeting's page. Who owes what from the
/// call is the page's follow-up card. Nothing shows for a meeting without
/// a doc.
struct MeetingDocCard: View {
    let sessionId: MeetingSessionId
    @Environment(AppModel.self) private var model
    @State private var store: LiveDocStore?

    var body: some View {
        // A stack, not a Group: with nothing in it yet, a Group never runs
        // its task, so the doc would never load.
        VStack(alignment: .leading, spacing: 0) {
            if let store, let doc = store.doc, store.watching == sessionId, !doc.points.isEmpty {
                PageSection("Doc from the call") {
                    DocPoints(store: store, doc: doc)
                    if let failure = store.failure { Text(failure).metaText(Theme.inkSecondary) }
                }
                .padding(.bottom, 16)
            }
        }
        .task(id: sessionId) {
            let store = store ?? LiveDocStore(core: model.core)
            self.store = store
            store.watch(sessionId)
        }
    }
}

/// The recap, copied, opened in Mail or posted to Slack, the ways a
/// meeting's follow-up goes. Only ever on the user's click.
struct RecapShareMenu: View {
    let text: String
    @Environment(AppModel.self) private var model

    var body: some View {
        let connections = model.connections.connections
        let slack = connections.filter { $0.kind == .slackWebhook || $0.kind == .slackBot }
        HStack(spacing: 8) {
            Menu("Share recap") {
                Button("Copy") { send(.copy, connections) }
                Button("Open in Mail") { send(.mail, connections) }
                if !slack.isEmpty { Divider() }
                ForEach(slack) { connection in
                    Button("Post to \(connection.name)") { send(.slack(connection.id), connections) }
                }
            }
            .menuStyle(.button)
            .fixedSize()
            if let notice = model.heard.notice { Text(notice).metaText() }
        }
        .task { await model.connections.load() }
    }

    private func send(_ route: FollowUpRoute, _ connections: [SonaConnection]) {
        let title = model.live.live?.session.title ?? ""
        model.heard.send(text, subject: title.isEmpty ? "Recap" : "Recap: \(title)", via: route, connections)
    }
}

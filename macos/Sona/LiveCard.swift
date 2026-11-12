import AppKit
import SwiftUI

/// `MeetingLivePromise`: one promise heard while a meeting records.
struct MeetingLivePromise: Decodable, Identifiable, Equatable {
    let id: String
    var what: String
    var owner: WorkOwner
    var ownerName: String?
    var ownerSet: Bool
    var dueAtMs: Int64?
    var dueWords: String?
    let quote: String
    let startOffsetNs: Int64
    let endOffsetNs: Int64
    /// The user changed its words or date, so later readings leave them.
    var edited: Bool
}

/// `MeetingLiveCard`: the promises the running capture has heard.
struct MeetingLiveCard: Decodable, Equatable {
    let sessionId: MeetingSessionId
    var promises: [MeetingLivePromise]
    let modelRequests: Int
    let heardThroughNs: Int64
}

/// `WorkFollowUp`: who owes what from one meeting, ready to send.
struct WorkFollowUp: Decodable, Equatable {
    let meetingId: String
    let text: String
    let lines: [WorkFollowUpLine]
}

struct WorkFollowUpLine: Decodable, Equatable, Identifiable {
    let itemId: String
    let ownerName: String
    let title: String
    let dueAtMs: Int64?
    let quote: String?
    var id: String { itemId }
}

private struct LiveOwnerParams: Encodable {
    let sessionId: MeetingSessionId
    let promiseId: String
    let owner: WorkOwner
}

private struct LiveEditParams: Encodable {
    let sessionId: MeetingSessionId
    let promiseId: String
    let what: String
    let dueAtMs: Int64?
}

private struct FollowUpPostParams: Encodable {
    let connectionId: String
    let operationId: String
    let text: String
}

/// Where ⌘↩ takes a meeting's follow-up. Nothing goes anywhere on its own.
enum FollowUpRoute: Hashable {
    case copy
    case mail
    case slack(String)

    init(stored: String) {
        if stored == "mail" {
            self = .mail
        } else if stored.hasPrefix("slack:") {
            self = .slack(String(stored.dropFirst("slack:".count)))
        } else {
            self = .copy
        }
    }

    var stored: String {
        switch self {
        case .copy: "copy"
        case .mail: "mail"
        case let .slack(id): "slack:\(id)"
        }
    }
}

/// `"Dana · Fri 7 Nov"`: who owes a promise and by when.
enum PromiseLabel {
    static func text(owner: String?, dueAtMs: Int64?) -> String {
        let who = owner ?? "No owner"
        guard let dueAtMs else { return who }
        return "\(who) · \(day(dueAtMs))"
    }

    static func day(_ ms: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
            .formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated))
    }
}

/// The promises heard in the running meeting, and the follow-up of the
/// meeting whose triage pass is on screen.
@MainActor
@Observable
final class LiveCardStore {
    private(set) var card: MeetingLiveCard?
    private(set) var failure: String?
    /// Bumped to put the cursor in "What did we decide on…".
    private(set) var askRequests = 0
    /// The running meeting a live screen shows.
    private(set) var watching: MeetingSessionId?
    /// The meeting whose triage pass is on screen, for its keys.
    private(set) var pass: MeetingSessionId?
    var highlighted: String?
    private(set) var followUp: WorkFollowUp?
    private(set) var followUpFailure: String?
    private(set) var notice: String?
    var route: FollowUpRoute {
        didSet { UserDefaults.standard.set(route.stored, forKey: Self.routeKey) }
    }

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var ownerEdits = 0

    private static let routeKey = "followUpRoute"

    init(core: Core) {
        self.core = core
        route = FollowUpRoute(stored: UserDefaults.standard.string(forKey: Self.routeKey) ?? "copy")
        core.observe(CoreEvent.meetingLiveCardChanged) { [weak self] line in
            guard let self, let payload: MeetingEventPayload = try? Core.payload(line),
                  payload.sessionId == self.watching else { return }
            Task { await self.load() }
        }
        core.observe("work-changed") { [weak self] _ in
            guard let self, self.pass != nil else { return }
            Task { await self.loadFollowUp() }
        }
    }

    // MARK: While recording

    func watch(_ sessionId: MeetingSessionId?) {
        guard watching != sessionId else { return }
        watching = sessionId
        card = nil
        failure = nil
        Task { await load() }
    }

    func load() async {
        guard let sessionId = watching, ownerEdits == 0 else { return }
        do {
            let result: MeetingLiveCard? = try await core.request("meeting_live_card", ["sessionId": sessionId])
            guard watching == sessionId, ownerEdits == 0 else { return }
            card = result
            failure = nil
        } catch {
            guard watching == sessionId else { return }
            failure = "Promises heard could not be read."
        }
    }

    /// Who owes a promise, shown at once. The recording is not touched.
    func setOwner(_ promise: MeetingLivePromise, _ owner: WorkOwner, name: String?) {
        guard let sessionId = card?.sessionId,
              let index = card?.promises.firstIndex(where: { $0.id == promise.id }) else { return }
        let before = card?.promises[index]
        card?.promises[index].owner = owner
        card?.promises[index].ownerName = name
        card?.promises[index].ownerSet = true
        ownerEdits += 1
        Task {
            defer { ownerEdits -= 1 }
            do {
                let result: MeetingLiveCard = try await core.request(
                    "meeting_live_card_owner",
                    LiveOwnerParams(sessionId: sessionId, promiseId: promise.id, owner: owner))
                if ownerEdits == 1, watching == sessionId { card = result }
            } catch {
                if let before, let at = card?.promises.firstIndex(where: { $0.id == promise.id }) {
                    card?.promises[at] = before
                }
                failure = "That owner was not saved."
            }
        }
    }

    /// New words or a new date for a promise on the board, kept against
    /// later readings.
    func edit(_ promise: MeetingLivePromise, what: String, dueAtMs: Int64?) {
        let what = what.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let sessionId = card?.sessionId, !what.isEmpty,
              what != promise.what || dueAtMs != promise.dueAtMs,
              let index = card?.promises.firstIndex(where: { $0.id == promise.id }) else { return }
        let before = card?.promises[index]
        card?.promises[index].what = what
        card?.promises[index].dueAtMs = dueAtMs
        card?.promises[index].edited = true
        ownerEdits += 1
        Task {
            defer { ownerEdits -= 1 }
            do {
                let result: MeetingLiveCard = try await core.request(
                    "meeting_live_card_edit",
                    LiveEditParams(sessionId: sessionId, promiseId: promise.id, what: what, dueAtMs: dueAtMs))
                if ownerEdits == 1, watching == sessionId { card = result }
            } catch {
                if let before, let at = card?.promises.firstIndex(where: { $0.id == promise.id }) {
                    card?.promises[at] = before
                }
                failure = "That change was not saved."
            }
        }
    }

    func requestAsk() { askRequests += 1 }

    /// "What did we decide on X?", answered from the words heard so far in
    /// the call's own help panel.
    func askDecided(_ topic: String, _ live: MeetingLiveStore) {
        let topic = topic.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !topic.isEmpty else { return }
        live.setHelpQuestion("What did we decide on \(topic)?")
        live.askLiveHelp(.ask)
    }

    // MARK: After the call

    func showPass(_ sessionId: MeetingSessionId?) {
        guard pass != sessionId else { return }
        pass = sessionId
        highlighted = nil
        followUp = nil
        followUpFailure = nil
        notice = nil
        Task { await loadFollowUp() }
    }

    func loadFollowUp() async {
        guard let sessionId = pass else { return }
        do {
            let result: WorkFollowUp = try await core.request("work_follow_up", ["meetingId": sessionId])
            guard pass == sessionId else { return }
            followUp = result
            followUpFailure = nil
        } catch {
            guard pass == sessionId else { return }
            followUpFailure = "The follow-up could not be written."
        }
    }

    /// The follow-up, where the person chose. Only ever on their keypress
    /// or click.
    func deliver(subject: String, _ connections: [SonaConnection]) {
        guard let text = followUp?.text, !text.isEmpty else { return }
        send(text, subject: subject, via: route, connections)
    }

    /// Any text from a meeting, copied, opened in Mail or posted to Slack.
    func send(_ text: String, subject: String, via route: FollowUpRoute, _ connections: [SonaConnection]) {
        switch route {
        case .copy:
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(text, forType: .string)
            notice = "Copied."
        case .mail:
            var mail = URLComponents()
            mail.scheme = "mailto"
            mail.queryItems = [URLQueryItem(name: "subject", value: subject), URLQueryItem(name: "body", value: text)]
            if let url = mail.url, NSWorkspace.shared.open(url) {
                notice = "Opened in Mail."
            } else {
                notice = "Mail could not open it."
            }
        case let .slack(id):
            guard let connection = connections.first(where: { $0.id == id }) else {
                notice = "That Slack connection is gone. Choose another."
                return
            }
            notice = "Posting to \(connection.name)…"
            Task {
                do {
                    let receipt: ConnectionReceipt = try await core.request(
                        "work_follow_up_post",
                        FollowUpPostParams(connectionId: id, operationId: UUID().uuidString, text: text))
                    notice = receipt.state == "sent" ? "Posted to \(connection.name)." : receipt.detail
                } catch {
                    notice = error.localizedDescription
                }
            }
        }
    }

    /// The meeting's tasks still waiting in triage, in the order they were
    /// said.
    func passRows(_ work: WorkStore) -> [WorkItemRow] {
        guard let pass else { return [] }
        return Self.rows(of: pass, in: work)
    }

    static func rows(of sessionId: MeetingSessionId, in work: WorkStore) -> [WorkItemRow] {
        (work.items ?? [])
            .filter { $0.sourceMeetingId == sessionId && $0.status == .triage && $0.archivedAtMs == nil }
            .sorted { ($0.sourceAtMs ?? .max, $0.number) < ($1.sourceAtMs ?? .max, $1.number) }
    }
}

/// The keys of the live card and the triage pass, on the meetings screen.
@MainActor
enum LiveCardKeys {
    static func handle(_ event: NSEvent, key: String, flags: NSEvent.ModifierFlags, _ model: AppModel) -> Bool {
        let heard = model.heard
        if let session = model.live.live?.session, session.phase.isActive,
           heard.watching == session.sessionId, flags.isEmpty, key == "d" {
            heard.requestAsk()
            return true
        }
        guard heard.pass != nil else { return false }
        let rows = heard.passRows(model.work)
        if flags == .command, event.keyCode == 36 {
            heard.deliver(subject: subject(model), model.connections.connections)
            return true
        }
        guard flags.isEmpty || flags == .function, !rows.isEmpty else { return false }
        let at = rows.firstIndex { $0.id == heard.highlighted }
        switch (key, event.keyCode) {
        case ("j", _), (_, 125):
            heard.highlighted = rows[min((at ?? -1) + 1, rows.count - 1)].id
        case ("k", _), (_, 126):
            heard.highlighted = rows[max((at ?? rows.count) - 1, 0)].id
        case ("1", _):
            answer(.accept, rows, at, model)
        case ("2", _):
            answer(.decline, rows, at, model)
        default:
            return false
        }
        return true
    }

    private static func answer(_ action: WorkAction, _ rows: [WorkItemRow], _ at: Int?, _ model: AppModel) {
        let row = rows[at ?? 0]
        let rest = rows.filter { $0.id != row.id }
        model.heard.highlighted = rest.isEmpty ? nil : rest[min(at ?? 0, rest.count - 1)].id
        model.perform(action, on: [row.id])
    }

    private static func subject(_ model: AppModel) -> String {
        guard let title = model.meetings.snapshot?.session.title, !title.isEmpty else { return "Follow-up" }
        return "Follow-up: \(title)"
    }
}

/// Who owes one promise, as a chip that picks them. Picking changes only
/// the card: the recording keeps going. On the board the date has its own
/// chip, so the owner chip leaves it out.
private struct PromiseOwnerMenu: View {
    let promise: MeetingLivePromise
    var showsDay = true
    @Environment(AppModel.self) private var model

    var body: some View {
        Menu {
            Button("Me") { model.heard.setOwner(promise, .myself, name: "Me") }
            if !model.work.people.isEmpty {
                Divider()
                ForEach(model.work.people) { person in
                    Button(person.name) { model.heard.setOwner(promise, .person(person.id), name: person.name) }
                }
            }
            Divider()
            Button("No owner") { model.heard.setOwner(promise, .nobody, name: nil) }
        } label: {
            Text(PromiseLabel.text(owner: promise.ownerName, dueAtMs: showsDay ? promise.dueAtMs : nil))
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(promise.ownerName == nil ? Theme.inkTertiary : Theme.inkSecondary)
                .lineLimit(1)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .padding(.horizontal, 6)
        .frame(height: 18)
        .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
        .help("Who owes this")
        .accessibilityLabel("Owner: \(promise.ownerName ?? "none"). Change")
    }
}

/// When a promise is due, as a chip that opens a calendar.
private struct PromiseDueButton: View {
    let promise: MeetingLivePromise
    @Environment(AppModel.self) private var model
    @State private var open = false
    @State private var day = Date()

    var body: some View {
        Button {
            day = promise.dueAtMs.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000) } ?? Date()
            open = true
        } label: {
            Text(promise.dueAtMs.map(PromiseLabel.day) ?? "No date")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(promise.dueAtMs == nil ? Theme.inkTertiary : Theme.inkSecondary)
                .lineLimit(1)
        }
        .buttonStyle(.plain)
        .fixedSize()
        .padding(.horizontal, 6)
        .frame(height: 18)
        .background(Theme.control, in: RoundedRectangle(cornerRadius: Theme.radiusKey))
        .help("When this is due")
        .accessibilityLabel("Due: \(promise.dueAtMs.map(PromiseLabel.day) ?? "no date"). Change")
        .popover(isPresented: $open) {
            VStack(alignment: .leading, spacing: 12) {
                DatePicker("Due", selection: $day, displayedComponents: .date)
                    .datePickerStyle(.graphical)
                    .labelsHidden()
                HStack {
                    Button("No date") { save(nil) }.buttonStyle(.quiet)
                    Spacer()
                    Button("Set") { save(day) }.buttonStyle(.primary)
                }
            }
            .padding(16)
        }
    }

    /// Five in the afternoon local time, the hour the core gives a day heard
    /// in the call.
    private func save(_ day: Date?) {
        open = false
        let due = day.map { date -> Int64 in
            let end = Calendar.current.date(bySettingHour: 17, minute: 0, second: 0, of: date) ?? date
            return Int64(end.timeIntervalSince1970 * 1000)
        }
        model.heard.edit(promise, what: promise.what, dueAtMs: due)
    }
}

/// One promise's words, edited in place; kept on return or when the field
/// loses focus.
private struct PromiseWhatField: View {
    let promise: MeetingLivePromise
    @Environment(AppModel.self) private var model
    @State private var draft = ""
    @FocusState private var focused: Bool

    var body: some View {
        TextField("What", text: $draft, axis: .vertical)
            .textFieldStyle(.plain)
            .font(TypeScale.body())
            .foregroundStyle(Theme.ink)
            .lineLimit(1...3)
            .focused($focused)
            .onSubmit(save)
            .onAppear { draft = promise.what }
            .onChange(of: promise.what) { if !focused { draft = promise.what } }
            .onChange(of: focused) { if !focused { save() } }
            .accessibilityLabel("What is owed")
    }

    private func save() {
        model.heard.edit(promise, what: draft, dueAtMs: promise.dueAtMs)
    }
}

/// The newest promises, in the recording's own card.
struct LiveCardStrip: View {
    let sessionId: MeetingSessionId
    @Environment(AppModel.self) private var model

    var body: some View {
        let promises = model.heard.watching == sessionId ? model.heard.card?.promises ?? [] : []
        Group {
            if !promises.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Hairline()
                    ForEach(promises.suffix(3)) { promise in
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text(promise.what)
                                .font(TypeScale.label(13))
                                .foregroundStyle(Theme.ink)
                                .lineLimit(1)
                            Spacer(minLength: 4)
                            PromiseOwnerMenu(promise: promise)
                        }
                    }
                    if promises.count > 3 {
                        Text("\(promises.count - 3) more heard").metaText()
                    }
                }
            }
        }
        .onAppear { model.heard.watch(sessionId) }
    }
}

/// The board: who owes what by when, heard so far and edited in place, at
/// the head of the live screen's side column, and the question that reads
/// it back. Soonest due first; undated promises after, in the order heard.
struct LiveCardSection: View {
    let sessionId: MeetingSessionId
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var topic = ""
    @FocusState private var asking: Bool

    var body: some View {
        let heard = model.heard
        let promises = (heard.watching == sessionId ? heard.card?.promises ?? [] : [])
            .enumerated()
            .sorted { ($0.element.dueAtMs ?? .max, $0.offset) < ($1.element.dueAtMs ?? .max, $1.offset) }
            .map(\.element)
        PageSection("Board") {
            if promises.isEmpty {
                Text("Who owes what by when shows here as it is said.").metaText(Theme.inkTertiary)
            } else {
                Card {
                    ForEach(promises) { promise in
                        VStack(alignment: .leading, spacing: 6) {
                            PromiseWhatField(promise: promise)
                            HStack(spacing: 6) {
                                PromiseOwnerMenu(promise: promise, showsDay: false)
                                PromiseDueButton(promise: promise)
                                Spacer(minLength: 0)
                            }
                            Text("“\(promise.quote)”").metaText().lineLimit(2)
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                        .overlay(alignment: .bottom) { Hairline().padding(.horizontal, 16) }
                    }
                }
                .animation(Motion.settle(reduced: reduceMotion), value: promises.map(\.id))
            }
            HStack(spacing: 8) {
                TextField("What did we decide on…", text: $topic,
                          prompt: Text("What did we decide on…").foregroundStyle(Theme.inkTertiary))
                    .textFieldStyle(.plain)
                    .font(TypeScale.body())
                    .foregroundStyle(Theme.ink)
                    .focused($asking)
                    .onSubmit(ask)
                    .padding(.horizontal, 12)
                    .frame(height: Theme.controlHeight)
                    .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radiusControl))
                    .overlay(RoundedRectangle(cornerRadius: Theme.radiusControl)
                        .strokeBorder(asking ? Theme.accent : Theme.border, lineWidth: 1))
                    .accessibilityLabel("What did we decide on")
                Shortcut("d")
            }
            if let failure = heard.failure, heard.watching == sessionId {
                Text(failure).metaText(Theme.inkSecondary)
            }
        }
        .onAppear { heard.watch(sessionId) }
        .onChange(of: heard.askRequests) { asking = true }
    }

    private func ask() {
        model.heard.askDecided(topic, model.live)
        topic = ""
    }
}

/// A finished meeting's promises: its triage pass, and the follow-up that
/// says who owes what by when.
struct HeardPromisesCard: View {
    let sessionId: MeetingSessionId
    @Environment(AppModel.self) private var model

    var body: some View {
        let heard = model.heard
        let rows = LiveCardStore.rows(of: sessionId, in: model.work)
        let followUp = heard.pass == sessionId ? heard.followUp : nil
        Group {
            if !rows.isEmpty || !(followUp?.text.isEmpty ?? true) {
                PageSection("Promises") {
                    if !rows.isEmpty {
                        Card {
                            ForEach(rows) { row in pending(row, highlighted: heard.highlighted == row.id) }
                        }
                    }
                    if let followUp, !followUp.text.isEmpty {
                        draft(followUp)
                    }
                    if let failure = heard.followUpFailure { Text(failure).metaText(Theme.inkSecondary) }
                }
                .padding(.bottom, 16)
            }
        }
        .onAppear {
            heard.showPass(sessionId)
            Task { await model.connections.load() }
        }
        .onDisappear { if heard.pass == sessionId { heard.showPass(nil) } }
    }

    private func pending(_ row: WorkItemRow, highlighted: Bool) -> some View {
        CardRow(action: { model.heard.highlighted = row.id }) {
            VStack(alignment: .leading, spacing: 4) {
                Text(row.title).bodyText(14).lineLimit(2)
                if let quote = row.sourceQuote {
                    Text("“\(quote)”").metaText().lineLimit(2)
                }
            }
        } trailing: {
            HStack(spacing: 8) {
                Chip(PromiseLabel.text(owner: row.ownerIsSelf ? "Me" : row.ownerName, dueAtMs: row.dueAtMs))
                Button("Accept") { model.perform(.accept, on: [row.id]) }.buttonStyle(.quiet).help("Accept  1")
                Button("Decline") { model.perform(.decline, on: [row.id]) }.buttonStyle(.quiet).help("Decline  2")
            }
        }
        .background(highlighted ? Theme.wash : .clear)
    }

    private func draft(_ followUp: WorkFollowUp) -> some View {
        let heard = model.heard
        let slack = model.connections.connections.filter { $0.kind == .slackWebhook || $0.kind == .slackBot }
        return Card {
            VStack(alignment: .leading, spacing: 12) {
                Text(followUp.text)
                    .bodyText(13, Theme.inkSecondary)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 8) {
                    Picker("Send with", selection: Binding(get: { heard.route }, set: { heard.route = $0 })) {
                        Text("Copy").tag(FollowUpRoute.copy)
                        Text("Mail").tag(FollowUpRoute.mail)
                        ForEach(slack) { connection in
                            Text(connection.name).tag(FollowUpRoute.slack(connection.id))
                        }
                    }
                    .labelsHidden()
                    .fixedSize()
                    if let notice = heard.notice { Text(notice).metaText() }
                    Spacer(minLength: 8)
                    Button(action: { heard.deliver(subject: subject, model.connections.connections) }) {
                        HStack(spacing: 6) {
                            Text(label(heard.route, slack))
                            Text("⌘↩").opacity(0.7)
                        }
                    }
                    .buttonStyle(.primary)
                }
            }
            .padding(16)
        }
    }

    private var subject: String {
        guard let title = model.meetings.snapshot?.session.title, !title.isEmpty else { return "Follow-up" }
        return "Follow-up: \(title)"
    }

    private func label(_ route: FollowUpRoute, _ slack: [SonaConnection]) -> String {
        switch route {
        case .copy: "Copy follow-up"
        case .mail: "Open in Mail"
        case let .slack(id): "Post to \(slack.first { $0.id == id }?.name ?? "Slack")"
        }
    }
}

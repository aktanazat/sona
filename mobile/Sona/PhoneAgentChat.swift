import Foundation

/// One line of a chat with Sona's agent, as this iPhone keeps it.
struct AgentTurn: Codable, Identifiable, Equatable {
    enum Role: String, Codable { case user, assistant }

    /// Where a question is between this iPhone and the Mac that answers it.
    enum Status: Codable, Equatable {
        /// Still in this iPhone's outbox.
        case queued
        /// In the vault since `at`; the Mac has not taken it yet.
        case sent(at: Date)
        /// The Mac took it at `at` and is asking the agent.
        case working(at: Date)
        case answered
        /// The Mac could not answer; the code is the wire's `failure`.
        case failed(String)
        /// The Mac did not take it, or did not finish, in the time it normally needs.
        /// Still watched: a late answer lands here.
        case unanswered
    }

    let id: UUID
    var role: Role
    var text: String
    var date: Date
    var askObjectId: String?
    var status: Status?

    var isWaiting: Bool {
        switch status {
        case .queued, .sent, .working, .unanswered: return true
        case .answered, .failed, nil: return false
        }
    }
}

struct AgentConversation: Codable, Identifiable, Equatable {
    let id: UUID
    var turns: [AgentTurn]
    var updated: Date

    var title: String { turns.first { $0.role == .user }?.text ?? "" }
}

/// What the chat needs from the vault: send a question, follow it through the outbox,
/// take back one that is not going to leave, read the Mac's replies, and delete an
/// exchange from the vault.
@MainActor
protocol AgentTransport: AnyObject {
    func send(_ ask: QueuedAsk, askedAt: Date) async throws -> String
    func outbox(_ askObjectId: String) async -> AskOutbox
    /// False while the question is being sent: it may still reach the vault.
    func withdraw(_ askObjectId: String) async -> Bool
    func replies() async throws -> [String: AgentReplyManifest]
    /// Throws until the ask is in this iPhone's copy of the vault and its delete went through.
    func deleteAsk(_ askObjectId: String) async throws
    /// Throws until the reply is in this iPhone's copy of the vault and its delete went through.
    func deleteReply(_ askObjectId: String) async throws
}

/// The phone's chat with Sona's agent. The Mac answers, so every question has a clock:
/// one the Mac does not take or finish in time says so instead of spinning.
@MainActor
final class PhoneAgentChat: ObservableObject {
    /// How long the Mac normally needs to take a question that reached the vault: it
    /// checks every few seconds while it is awake.
    static let pickUpWindow: TimeInterval = 45
    /// How long the Mac's agent turn may run, with room for the reply to arrive.
    static let answerWindow: TimeInterval = 300
    /// How long a question may wait in this iPhone's outbox while nothing is uploading
    /// before it counts as not sent: long enough for a slow connection, short enough to
    /// offer Try again. An upload in progress pauses it.
    static let sendWindow: TimeInterval = 120

    @Published private(set) var conversations: [AgentConversation] = []
    @Published var refusal: AgentAskObject.Refusal?
    /// Called once per answer or failure, with the conversation and the turn.
    var onSettled: ((AgentConversation, AgentTurn) -> Void)?

    private let transport: AgentTransport
    private let file: URL
    private let now: () -> Date
    private var watcher: Task<Void, Never>?
    /// When each queued question's send clock last restarted: when it was asked, or
    /// when the outbox last reported an upload in progress.
    private var sendClock: [UUID: Date] = [:]
    /// Questions taken off the screen while they were being sent. Each may still reach
    /// the vault, so its ask and reply are deleted there once they show up.
    private(set) var orphans: [Orphan] = []

    struct Orphan: Codable, Equatable {
        let askObjectId: String
        let since: Date
        var askDeleted = false
    }

    init(transport: AgentTransport, file: URL, now: @escaping () -> Date = Date.init) {
        self.transport = transport
        self.file = file
        self.now = now
        if let bytes = try? Data(contentsOf: file),
           let stored = try? JSONDecoder().decode([AgentConversation].self, from: bytes)
        {
            conversations = stored
        }
        if let bytes = try? Data(contentsOf: orphanFile),
           let stored = try? JSONDecoder().decode([Orphan].self, from: bytes)
        {
            orphans = stored
        }
    }

    private var orphanFile: URL { file.appendingPathExtension("withdrawn") }

    var isWaiting: Bool { conversations.contains { $0.turns.contains(where: \.isWaiting) } }

    /// Sends `question` into `conversationID`, or a new conversation, and returns the
    /// conversation and the question's turn. Nil when the Mac's turn would refuse it.
    @discardableResult
    func ask(_ question: String, in conversationID: UUID? = nil) async -> (conversation: UUID, turn: UUID)? {
        let text = question.trimmingCharacters(in: .whitespacesAndNewlines)
        if let refusal = AgentAskObject.refusal(text) {
            self.refusal = refusal
            return nil
        }
        refusal = nil
        let earlier = conversationID.flatMap { id in conversations.first { $0.id == id } }?.turns ?? []
        let recent = earlier.filter { $0.role == .assistant || $0.status == .answered }.map {
            AgentAskManifest.Turn(message: $0.text, role: $0.role == .user ? .user : .assistant)
        }
        let conversation = conversationID ?? UUID()
        let turn = AgentTurn(id: UUID(), role: .user, text: text, date: now(), askObjectId: nil, status: .queued)
        if let index = conversations.firstIndex(where: { $0.id == conversation }) {
            conversations[index].turns.append(turn)
            conversations[index].updated = turn.date
        } else {
            conversations.insert(AgentConversation(id: conversation, turns: [turn], updated: turn.date), at: 0)
        }
        save()
        let ask = QueuedAsk(conversationId: conversation.uuidString, question: text, recentTurns: recent)
        do {
            let objectId = try await transport.send(ask, askedAt: turn.date)
            update(turn.id) { $0.askObjectId = objectId }
            sendClock[turn.id] = turn.date
        } catch {
            update(turn.id) { $0.status = .failed("not_sent") }
            settle(turn.id, reply: find(turn.id))
        }
        watch()
        return (conversation, turn.id)
    }

    /// Sends a failed or unanswered question again, as a new turn after it.
    func retry(_ turn: AgentTurn, in conversationID: UUID) async {
        await ask(turn.text, in: conversationID)
    }

    /// Removes the conversation. Questions still in the outbox are taken back; any that
    /// reached the vault, or may still reach it, are followed until their ask and reply
    /// are deleted there.
    func delete(_ conversationID: UUID) async {
        let open = conversations.first { $0.id == conversationID }?.turns.filter(\.isWaiting) ?? []
        conversations.removeAll { $0.id == conversationID }
        save()
        for turn in open {
            guard let objectId = turn.askObjectId else { continue }
            if turn.status == .queued, await transport.withdraw(objectId) { continue }
            orphans.append(Orphan(askObjectId: objectId, since: now()))
        }
        saveOrphans()
        watch()
    }

    /// The settled question `turnID`, waiting at most `timeout`.
    func settled(_ turnID: UUID, timeout: TimeInterval) async -> AgentTurn? {
        let deadline = now().addingTimeInterval(timeout)
        while now() < deadline {
            if let turn = find(turnID), !turn.isWaiting || turn.status == .unanswered { return turn }
            try? await Task.sleep(for: .seconds(1))
        }
        return find(turnID)
    }

    /// Keeps checking while any question waits: every two seconds while the Mac is
    /// expected to act, every fifteen once it is overdue.
    func watch() {
        guard watcher == nil, isWaiting || !orphans.isEmpty else { return }
        watcher = Task { [weak self] in
            while let self, !Task.isCancelled, self.isWaiting || !self.orphans.isEmpty {
                await self.check()
                let overdue = self.conversations.allSatisfy { conversation in
                    conversation.turns.allSatisfy { !$0.isWaiting || $0.status == .unanswered }
                }
                try? await Task.sleep(for: .seconds(overdue ? 15 : 2))
            }
            self?.watcher = nil
        }
    }

    /// One look at the outbox and the vault.
    func check() async {
        await followOrphans()
        for turn in waiting where turn.status == .queued {
            guard let objectId = turn.askObjectId else { continue }
            switch await transport.outbox(objectId) {
            case .left:
                sendClock[turn.id] = nil
                update(turn.id) { $0.status = .sent(at: now()) }
            case .refused:
                await notSent(turn, objectId)
            case .busy:
                sendClock[turn.id] = now()
            case .waiting:
                let since = sendClock[turn.id] ?? now()
                sendClock[turn.id] = since
                if now().timeIntervalSince(since) > Self.sendWindow { await notSent(turn, objectId) }
            }
        }
        if waiting.contains(where: { $0.status != .queued }), let replies = try? await transport.replies() {
            for turn in waiting {
                guard let objectId = turn.askObjectId, let reply = replies[objectId] else { continue }
                await apply(reply, to: turn)
            }
        }
        let current = now()
        for turn in waiting {
            switch turn.status {
            case let .sent(at) where current.timeIntervalSince(at) > Self.pickUpWindow,
                 let .working(at) where current.timeIntervalSince(at) > Self.answerWindow:
                update(turn.id) { $0.status = .unanswered }
            default: break
            }
        }
    }

    /// Takes the question back from the outbox and offers Try again. One being sent stays
    /// as it is: it is about to leave.
    private func notSent(_ turn: AgentTurn, _ objectId: String) async {
        guard await transport.withdraw(objectId) else { return }
        sendClock[turn.id] = nil
        update(turn.id) { $0.status = .failed("not_sent") }
        settle(turn.id, reply: find(turn.id))
    }

    /// An orphan still in the outbox is taken back once it is no longer being sent. One
    /// in the vault has its ask deleted at once, so a Mac that has not started drops
    /// it, and its reply when the Mac finishes. Each delete counts only once it went
    /// through; until then it is tried again. An orphan whose ask is gone is let go
    /// after the Mac's own windows if no final reply came.
    private func followOrphans() async {
        guard !orphans.isEmpty else { return }
        let replies = try? await transport.replies()
        var kept: [Orphan] = []
        for var orphan in orphans {
            let objectId = orphan.askObjectId
            switch await transport.outbox(objectId) {
            case .waiting, .busy, .refused:
                if await transport.withdraw(objectId) { continue }
            case .left:
                if !orphan.askDeleted, (try? await transport.deleteAsk(objectId)) != nil {
                    orphan.askDeleted = true
                }
                if let state = replies?[objectId]?.state, state != .working {
                    if orphan.askDeleted, (try? await transport.deleteReply(objectId)) != nil { continue }
                } else if orphan.askDeleted,
                          now().timeIntervalSince(orphan.since) > Self.pickUpWindow + Self.answerWindow
                {
                    continue
                }
            }
            kept.append(orphan)
        }
        if kept != orphans {
            orphans = kept
            saveOrphans()
        }
    }

    private func saveOrphans() {
        if orphans.isEmpty {
            try? FileManager.default.removeItem(at: orphanFile)
            return
        }
        try? JSONEncoder().encode(orphans)
            .write(to: orphanFile, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    private func apply(_ reply: AgentReplyManifest, to turn: AgentTurn) async {
        switch reply.state {
        case .working:
            if case .working = turn.status { return }
            update(turn.id) { $0.status = .working(at: now()) }
        case .answered:
            let answer = AgentTurn(
                id: UUID(), role: .assistant, text: reply.answer ?? "", date: now(),
                askObjectId: turn.askObjectId, status: nil
            )
            update(turn.id) { $0.status = .answered }
            insert(answer, after: turn.id)
            settle(turn.id, reply: answer)
        case .failed:
            update(turn.id) { $0.status = .failed(reply.failure ?? "failed") }
            settle(turn.id, reply: find(turn.id))
        }
        if reply.state != .working, let objectId = turn.askObjectId {
            try? await transport.deleteReply(objectId)
            try? await transport.deleteAsk(objectId)
        }
    }

    private func settle(_ turnID: UUID, reply: AgentTurn?) {
        guard let reply, let conversation = conversations.first(where: { $0.turns.contains { $0.id == turnID } })
        else { return }
        onSettled?(conversation, reply)
    }

    private var waiting: [AgentTurn] { conversations.flatMap(\.turns).filter(\.isWaiting) }

    private func find(_ turnID: UUID) -> AgentTurn? {
        conversations.lazy.flatMap(\.turns).first { $0.id == turnID }
    }

    private func update(_ turnID: UUID, _ change: (inout AgentTurn) -> Void) {
        for c in conversations.indices {
            guard let t = conversations[c].turns.firstIndex(where: { $0.id == turnID }) else { continue }
            change(&conversations[c].turns[t])
            save()
            return
        }
    }

    private func insert(_ answer: AgentTurn, after turnID: UUID) {
        for c in conversations.indices {
            guard let t = conversations[c].turns.firstIndex(where: { $0.id == turnID }) else { continue }
            conversations[c].turns.insert(answer, at: t + 1)
            conversations[c].updated = answer.date
            save()
            return
        }
    }

    private func save() {
        try? JSONEncoder().encode(conversations)
            .write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
}

/// How an answer from Sona's agent is told apart from the app's other notifications.
enum AgentNotification {
    static let identifierPrefix = "sona.agent."
    static let conversationKey = "sona.agent.conversation"
}

/// What to tell the person about a question that has no answer.
enum AgentFailure {
    /// The localization key for `status`, which is `.failed` or `.unanswered`.
    static func message(_ status: AgentTurn.Status?) -> String {
        switch status {
        case .unanswered: return "agent.status.unanswered"
        case .failed("agent_off"): return "agent.failure.agentOff"
        case .failed("unpaired"): return "agent.failure.unpaired"
        case .failed("unreachable"): return "agent.failure.unreachable"
        case .failed("timed_out"): return "agent.failure.timedOut"
        case .failed("not_sent"): return "agent.failure.notSent"
        default: return "agent.failure.failed"
        }
    }
}

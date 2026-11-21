import XCTest

/// The phone's half of asking Sona's agent through the paired Mac.
final class PhoneAgentTests: XCTestCase {
    private func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle(for: PhoneAgentTests.self).url(forResource: name, withExtension: "json"))
        return try Data(contentsOf: url)
    }

    /// The Mac parses these bytes with `deny_unknown_fields` and the same fixtures, so the
    /// phone has to write exactly them: sorted keys, unescaped slashes, nulls written out.
    func testAskAndReplyEncodeToTheSharedFixtures() throws {
        let askBytes = try fixture("agent-ask-v1")
        let ask = try JSONDecoder().decode(AgentAskManifest.self, from: askBytes)
        XCTAssertEqual(try AgentAskObject.encodeManifest(ask), askBytes)

        let replyBytes = try fixture("agent-reply-v1")
        let reply = try JSONDecoder().decode(AgentReplyManifest.self, from: replyBytes)
        XCTAssertEqual(reply.state, .answered)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        XCTAssertEqual(try encoder.encode(reply), replyBytes)
    }

    /// A reply counts only at the object id derived from its question.
    func testRepliesCountOnlyAtTheirDerivedObjectId() {
        var index = BoardIndex(vaultId: "vault")
        let reply = AgentReplyManifest(
            answer: "Yes", ask_object_id: "ask-1", device_id: "mac", failure: nil, format_version: 1,
            kind: AgentAskObject.replyKind, state: .answered, updated_at_utc_ms: 1
        )
        index.heads["somewhere-else"] = BoardHead(revisionId: "r1", chunkCount: 1, object: .agentReply(reply), sequence: 1)
        XCTAssertNil(index.agentReplies["ask-1"])
        index.heads[AgentAskObject.replyObjectId(askObjectId: "ask-1")] =
            BoardHead(revisionId: "r2", chunkCount: 1, object: .agentReply(reply), sequence: 2)
        XCTAssertEqual(index.agentReplies["ask-1"], reply)
    }

    /// The Mac refuses these questions, so the phone refuses them before sending.
    @MainActor
    func testQuestionsTheMacWouldRefuseAreNotSent() async {
        let transport = FakeTransport()
        let chat = PhoneAgentChat(transport: transport, file: scratchFile())
        let asked = await chat.ask("Open https://example.com")
        XCTAssertNil(asked)
        XCTAssertEqual(chat.refusal, .address)
        XCTAssertTrue(transport.sent.isEmpty)
    }

    /// A Mac that is asleep never takes the question; the chat says so after the pick-up
    /// window instead of waiting forever, and a late answer still lands once.
    @MainActor
    func testUnpickedQuestionBecomesUnansweredThenTakesALateAnswer() async throws {
        let transport = FakeTransport()
        var clock = Date(timeIntervalSince1970: 1_000)
        let chat = PhoneAgentChat(transport: transport, file: scratchFile(), now: { clock })
        let sent = await chat.ask("What did Nolan and I leave open?")
        let asked = try XCTUnwrap(sent)
        transport.outboxState = .left
        await chat.check()
        XCTAssertEqual(turn(chat, asked.turn)?.status, .sent(at: clock))

        clock += PhoneAgentChat.pickUpWindow + 1
        await chat.check()
        XCTAssertEqual(turn(chat, asked.turn)?.status, .unanswered)

        var settled: [AgentTurn] = []
        chat.onSettled = { _, turn in settled.append(turn) }
        transport.answers[transport.sent[0]] = AgentReplyManifest(
            answer: "The pricing deck.", ask_object_id: transport.sent[0], device_id: "mac", failure: nil,
            format_version: 1, kind: AgentAskObject.replyKind, state: .answered, updated_at_utc_ms: 2
        )
        await chat.check()
        await chat.check()
        let turns = try XCTUnwrap(chat.conversations.first?.turns)
        XCTAssertEqual(turns.map(\.role), [.user, .assistant])
        XCTAssertEqual(turns[0].status, .answered)
        XCTAssertEqual(turns[1].text, "The pricing deck.")
        XCTAssertEqual(settled.map(\.text), ["The pricing deck."])
        XCTAssertEqual(transport.forgotten, [transport.sent[0]])
    }

    /// A Mac that took the question but never finished is overdue after the answer
    /// window, not the shorter pick-up window.
    @MainActor
    func testWorkingQuestionBecomesUnansweredAfterTheAnswerWindow() async throws {
        let transport = FakeTransport()
        var clock = Date(timeIntervalSince1970: 1_000)
        let chat = PhoneAgentChat(transport: transport, file: scratchFile(), now: { clock })
        let sent = await chat.ask("Summarize my last meeting")
        let asked = try XCTUnwrap(sent)
        transport.outboxState = .left
        await chat.check()
        transport.answers[transport.sent[0]] = AgentReplyManifest(
            answer: nil, ask_object_id: transport.sent[0], device_id: "mac", failure: nil,
            format_version: 1, kind: AgentAskObject.replyKind, state: .working, updated_at_utc_ms: 2
        )
        await chat.check()
        let tookAt = clock
        XCTAssertEqual(turn(chat, asked.turn)?.status, .working(at: tookAt))

        clock += PhoneAgentChat.pickUpWindow + 1
        await chat.check()
        XCTAssertEqual(turn(chat, asked.turn)?.status, .working(at: tookAt))

        clock = tookAt + PhoneAgentChat.answerWindow + 1
        await chat.check()
        XCTAssertEqual(turn(chat, asked.turn)?.status, .unanswered)
    }

    /// A question stuck in the outbox (offline, or refused by the Worker) says so and
    /// offers Try again instead of "Sending" forever, is taken back so the Mac never
    /// answers it later, and is reported once, as the Action Button needs.
    @MainActor
    func testQuestionThatNeverLeavesFailsAndIsTakenBack() async throws {
        let transport = FakeTransport()
        var clock = Date(timeIntervalSince1970: 1_000)
        let chat = PhoneAgentChat(transport: transport, file: scratchFile(), now: { clock })
        var settled: [AgentTurn] = []
        chat.onSettled = { _, turn in settled.append(turn) }
        let sent = await chat.ask("Summarize my last meeting")
        let asked = try XCTUnwrap(sent)
        await chat.check()
        XCTAssertEqual(turn(chat, asked.turn)?.status, .queued)

        clock += PhoneAgentChat.sendWindow + 1
        await chat.check()
        XCTAssertEqual(turn(chat, asked.turn)?.status, .failed("not_sent"))
        XCTAssertEqual(transport.withdrawn, transport.sent)
        XCTAssertEqual(settled.map(\.id), [asked.turn])

        transport.outboxState = .refused
        let refused = await chat.ask("What is on tomorrow?", in: asked.conversation)
        let second = try XCTUnwrap(refused)
        await chat.check()
        XCTAssertEqual(turn(chat, second.turn)?.status, .failed("not_sent"))
        XCTAssertEqual(transport.withdrawn, transport.sent)
    }

    /// Deleting a conversation takes back its questions still in the outbox.
    @MainActor
    func testDeletingAConversationTakesBackItsQueuedQuestion() async throws {
        let transport = FakeTransport()
        let chat = PhoneAgentChat(transport: transport, file: scratchFile())
        let sent = await chat.ask("Summarize my last meeting")
        let asked = try XCTUnwrap(sent)
        await chat.delete(asked.conversation)
        XCTAssertTrue(chat.conversations.isEmpty)
        XCTAssertEqual(transport.withdrawn, transport.sent)
    }

    /// A question the outbox is sending cannot be taken back, so it is not marked not
    /// sent: it is about to leave.
    @MainActor
    func testQuestionBeingSentIsNotMarkedNotSent() async throws {
        let transport = FakeTransport()
        transport.sending = true
        var clock = Date(timeIntervalSince1970: 1_000)
        let chat = PhoneAgentChat(transport: transport, file: scratchFile(), now: { clock })
        let sent = await chat.ask("Summarize my last meeting")
        let asked = try XCTUnwrap(sent)
        clock += PhoneAgentChat.sendWindow + 1
        await chat.check()
        XCTAssertEqual(turn(chat, asked.turn)?.status, .queued)
        XCTAssertEqual(transport.withdrawn, [])
    }

    /// A deleted question that was being sent still reaches the vault; the chat keeps
    /// following it and deletes the ask at once and the reply once the Mac finishes.
    @MainActor
    func testDeletedQuestionThatStillLeavesIsDeletedFromTheVault() async throws {
        let transport = FakeTransport()
        transport.sending = true
        let file = scratchFile()
        let chat = PhoneAgentChat(transport: transport, file: file)
        let sent = await chat.ask("Summarize my last meeting")
        let asked = try XCTUnwrap(sent)
        await chat.delete(asked.conversation)
        XCTAssertEqual(chat.orphans.map(\.askObjectId), transport.sent)
        XCTAssertEqual(PhoneAgentChat(transport: transport, file: file).orphans, chat.orphans)

        transport.sending = false
        transport.outboxState = .left
        await chat.check()
        XCTAssertEqual(transport.forgotten, transport.sent)
        await chat.check()
        XCTAssertEqual(transport.forgotten, transport.sent)

        transport.answers[transport.sent[0]] = AgentReplyManifest(
            answer: "Done", ask_object_id: transport.sent[0], device_id: "mac", failure: nil,
            format_version: 1, kind: AgentAskObject.replyKind, state: .answered, updated_at_utc_ms: 2
        )
        await chat.check()
        XCTAssertEqual(transport.forgotten, transport.sent + transport.sent)
        XCTAssertEqual(chat.orphans, [])
    }

    /// A meeting holds other people's words, so iPhone search never shows it.
    func testMeetingsStayOutOfIPhoneSearch() {
        let note = { (kind: PhoneNote.Kind) in
            PhoneNote(id: UUID(), kind: kind, date: Date(), title: "Pricing", text: "Pricing deck")
        }
        XCTAssertEqual(PhoneNote.Kind.allCases.filter { note($0).isSearchable }, [.note, .dictation])
    }

    /// A note saved while the phone is locked cannot be written to the protected index;
    /// until a write succeeds it stays planned, so the write after unlocking sends it.
    func testANoteWhoseIndexWriteFailedIsSentAgain() {
        let saved = PhoneNote(id: UUID(), kind: .note, date: Date(), title: "Pilot", text: "Pilot in January")
        var book = SearchIndexBook()
        let locked = book.plan([saved])
        XCTAssertEqual(locked.changed, [saved])

        let unlocked = book.plan([saved])
        XCTAssertEqual(unlocked.changed, [saved])
        book.confirm(unlocked)
        XCTAssertTrue(book.plan([saved]).isEmpty)
        XCTAssertEqual(book.plan([]).removed, [saved.id])
    }

    @MainActor
    private func turn(_ chat: PhoneAgentChat, _ id: UUID) -> AgentTurn? {
        chat.conversations.flatMap(\.turns).first { $0.id == id }
    }

    private func scratchFile() -> URL {
        FileManager.default.temporaryDirectory.appending(path: "agent-chat-\(UUID().uuidString).json")
    }
}

@MainActor
private final class FakeTransport: AgentTransport {
    var sent: [String] = []
    var outboxState: AskOutbox = .waiting
    var withdrawn: [String] = []
    /// The outbox is sending every question, so none can be withdrawn.
    var sending = false
    var answers: [String: AgentReplyManifest] = [:]
    var forgotten: [String] = []

    func send(_ ask: QueuedAsk, askedAt: Date) async throws -> String {
        let id = "ask-\(sent.count)"
        sent.append(id)
        return id
    }

    func outbox(_ askObjectId: String) async -> AskOutbox { outboxState }

    func withdraw(_ askObjectId: String) async -> Bool {
        guard !sending else { return false }
        withdrawn.append(askObjectId)
        return true
    }

    func replies() async throws -> [String: AgentReplyManifest] { answers }

    func forget(_ askObjectId: String) async { forgotten.append(askObjectId) }
}

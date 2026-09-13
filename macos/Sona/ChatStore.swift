import AppKit
import Foundation
import Observation

/// The chat's state, which is the core's state read back.
///
/// The panel keeps one conversation, one live turn and at most one settings
/// proposal, and it is the only thing that may change them. So every command
/// here answers with the status it produced and that status is held whole:
/// nothing is patched locally, nothing is optimistic, and a status event only
/// ever means "read it again".
@MainActor
@Observable
final class ChatStore {
    private(set) var status: AgentPanelStatus?
    /// The titles the history menu lists, newest first, as the core orders them.
    private(set) var history: [AgentChatConversationSummary] = []
    private(set) var settings: ChatSettings?
    /// What the reader is typing. Held here so that opening another
    /// conversation can drop it.
    var draft = ""
    /// Which brain the next turn goes to.
    var workspace: AgentPanelWorkspace = .sonaChat
    /// A send, stop, apply, undo or history read is in flight.
    private(set) var busy = false
    private(set) var error: String?
    /// The pack the current Ask turn carried quoted at least one row, so the
    /// work line can say the corpus was read.
    private(set) var searchedCorpus = false
    /// Wall clock in milliseconds, ticked only while a turn runs. Elapsed
    /// numbers are the one thing on screen that moves with no event behind it.
    private(set) var now = ChatStore.milliseconds()

    @ObservationIgnored private let core: Core
    /// Which read is the newest. An event storm can start several, and only
    /// the last one's answer describes the present.
    @ObservationIgnored private var reads = 0
    @ObservationIgnored private var clock: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        for name in [
            CoreEvent.agentPanelStatusChanged,
            CoreEvent.agentPanelTurnChanged,
            CoreEvent.agentPanelProposalChanged,
        ] {
            core.observe(name) { [weak self] _ in
                Task { await self?.read() }
            }
        }
        core.observe(CoreEvent.chatSettingsChanged) { [weak self] _ in
            Task { await self?.readSettings() }
        }
    }

    func start() async {
        await readSettings()
        await read()
        await loadHistory()
    }

    // MARK: - What the views read

    var phase: ChatPhase { ChatPhase(status) }
    var conversation: [AgentChatTurn] { status?.conversation ?? [] }
    var turn: AgentPanelTurnStatus? { status?.turn }
    var proposal: AgentPanelProposal? { status?.proposal }
    var conversationId: String? { status?.conversationId }
    var rows: [ChatRow] { ChatRow.rows(conversation) }

    /// A turn that has not reached a terminal state. Stop is offered for
    /// exactly this, and the composer is closed for exactly this.
    var running: Bool { turn?.isRunning ?? false }

    /// The field and the send button are shut while a command is in flight,
    /// before the first status has arrived, and while the agent is off.
    var composerDisabled: Bool { busy || phase == .loading || phase == .disabled }

    var canSend: Bool {
        !composerDisabled && !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Where the turn's activity belongs in the scrollback: above the answer
    /// it produced, or at the end while there is no answer yet. `-1` when the
    /// turn has nothing to say about its work.
    var workRowIndex: Int {
        guard let turn else { return -1 }
        let hasWork = turn.isRunning
            || turn.completedAtUtcMs != nil
            || turn.failure != nil
            || searchedCorpus
        guard hasWork else { return -1 }
        let last = conversation.count - 1
        return last >= 0 && conversation[last].role == .assistant ? last : conversation.count
    }

    /// The row a settings answer is already showing, so the card replaces it
    /// instead of repeating it. `-1` when the card stands on its own.
    var proposalRowIndex: Int {
        guard let proposal else { return -1 }
        let last = conversation.count - 1
        guard last >= 0,
              conversation[last].role == .assistant,
              conversation[last].message == proposal.summary
        else { return -1 }
        return last
    }

    /// The question a live failure belongs to, which is the one a retry would
    /// ask again. Nil when the failure is not about the last thing said.
    var retryMessage: String? {
        guard turn?.failure != nil, let last = conversation.last, last.role == .user else {
            return nil
        }
        return last.message
    }

    /// Corpus evidence leaves this Mac only for an Ask turn, only when the
    /// panel is paired, and only under the switch that governs every other
    /// remote read of a recording.
    var packs: Bool {
        workspace == .sonaChat && isPaired && allowsRemoteIntelligence
    }

    /// The same three conditions with the switch off: the one case where the
    /// chat can ask for consent instead of answering badly.
    var consentNeeded: Bool {
        workspace == .sonaChat && isPaired && !allowsRemoteIntelligence
    }

    private var isPaired: Bool { settings?.isPaired ?? false }
    private var allowsRemoteIntelligence: Bool { settings?.allowsRemoteIntelligence ?? false }

    // MARK: - Reading

    /// Re-reads the whole panel. The notice's Retry is this and nothing else:
    /// a relay that came back is a status that reads differently.
    func reread() {
        Task { await read() }
    }

    private func read() async {
        reads += 1
        let read = reads
        do {
            let next: AgentPanelStatus = try await core.request("agent_panel_status")
            guard read == reads else { return }
            hold(next)
            error = nil
        } catch {
            guard read == reads else { return }
            /* A failed read keeps the last status: a stale conversation is
             * worth more than an empty window, and the line below says the
             * screen is not current. */
            report(error)
        }
    }

    func loadHistory() async {
        do {
            history = try await core.request("agent_chat_history_list")
            error = nil
        } catch {
            report(error)
        }
    }

    private func readSettings() async {
        do {
            settings = try await core.request("get_app_settings")
        } catch {
            report(error)
        }
    }

    // MARK: - Asking

    /// Sends what is in the field.
    func send() {
        let message = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty, !composerDisabled, !running else { return }
        Task {
            if await submit(message) {
                draft = ""
            }
        }
    }

    /// Asks the same question again after a failure. The field is untouched:
    /// the question is the row above, not what the reader was typing next.
    func retry(_ message: String) {
        guard !composerDisabled, !running else { return }
        Task { _ = await submit(message) }
    }

    private func submit(_ message: String) async -> Bool {
        /* The panel assembles no packs, by design: whoever asks decides what
         * evidence the question carries. Ask turns get the reader's own rows
         * quoted with `sona://` links; Configure turns get none, because a
         * settings change is not a question about a meeting. */
        let allowed = packs
        searchedCorpus = false
        /* Building the pack and sending the turn are one act, so they share
         * one closed composer: a second Return while the pack is still being
         * assembled would ask the same question twice. */
        let sent = await run {
            let pack = allowed ? await self.buildPack(message) : nil
            return try await self.core.request(
                "agent_panel_send_turn",
                Envelope(
                    request: SendTurnRequest(
                        turnId: UUID().uuidString,
                        message: message,
                        locale: self.settings?.language ?? "en",
                        workspace: self.workspace.rawValue,
                        contextPack: pack,
                        toolsAllowed: allowed)))
        }
        if sent {
            // A first question gives the conversation its title.
            await loadHistory()
        }
        return sent
    }

    /// The evidence this question carries, and whether it quoted anything.
    private func buildPack(_ message: String) async -> String? {
        do {
            let built: ChatContextPack = try await core.request(
                "sona_query_pack", ["question": message])
            searchedCorpus = !built.sources.isEmpty
            return built.pack
        } catch {
            /* A pack that could not be built is a poorer answer, not a
             * refusal: the turn still goes, with the tools the relay can call
             * for itself. */
            return nil
        }
    }

    /// Stops the live turn. What it had already done stays on screen, because
    /// it happened.
    func stop() {
        guard let turnId = turn?.turnId, !busy else { return }
        Task {
            _ = await run {
                try await self.core.request(
                    "agent_panel_cancel_turn", Envelope(request: TurnRequest(turnId: turnId)))
            }
        }
    }

    // MARK: - Settings proposals

    /// Applies the card. The revision it was built against rides along, so a
    /// proposal that describes settings which have since moved is refused by
    /// the core rather than applied over the top of them.
    func applyProposal() {
        guard let proposal, !busy else { return }
        Task {
            _ = await run {
                try await self.core.request(
                    "agent_panel_apply_change",
                    Envelope(
                        request: ApplyChangeRequest(
                            proposalId: proposal.proposalId,
                            expectedRevision: proposal.sourceSettingsRevision,
                            confirmed: true)))
            }
        }
    }

    /// Puts back what the apply changed, named by the receipt the apply
    /// produced.
    func undoProposal() {
        guard let proposal, let receiptId = proposal.receiptId, !busy else { return }
        let revision = proposal.undoRevision
        Task {
            _ = await run {
                try await self.core.request(
                    "agent_panel_undo_change",
                    Envelope(
                        request: UndoChangeRequest(
                            receiptId: receiptId, expectedRevision: revision)))
            }
        }
    }

    // MARK: - Corpus actions

    func applyAction(_ index: UInt32) {
        settle("agent_panel_apply_action", index)
    }

    func dismissAction(_ index: UInt32) {
        settle("agent_panel_dismiss_action", index)
    }

    /// An action's command answers with the turn alone, so the held status
    /// keeps everything else it already had.
    private func settle(_ method: String, _ index: UInt32) {
        guard let turnId = turn?.turnId, !busy else { return }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do {
                let next: AgentPanelTurnStatus = try await core.request(
                    method, Envelope(request: ActionRequest(turnId: turnId, actionIndex: index)))
                status?.turn = next
                tick()
            } catch {
                report(error)
            }
        }
    }

    // MARK: - Conversations

    func newChat() {
        guard !busy, !running else { return }
        draft = ""
        Task {
            if await run({ try await self.core.request("agent_chat_new") }) {
                /* The work line belonged to the turn that was on screen, and
                 * leaving the conversation refreshes the menu by itself. */
                searchedCorpus = false
            }
        }
    }

    func open(_ conversationId: String) {
        guard !busy, !running, conversationId != self.conversationId else { return }
        draft = ""
        Task {
            if await run({
                try await self.core.request("agent_chat_open", ["conversationId": conversationId])
            }) {
                // The work line belongs to the turn that was on screen, not
                // to whatever this conversation last did.
                searchedCorpus = false
            }
        }
    }

    // MARK: - Links

    /// Follows a `sona://` address out of the answer and into the thing it
    /// names. The core resolves it and focuses the window; a link to a row
    /// that has since been deleted resolves to nothing, and says so.
    func openLink(_ link: String) {
        Task {
            do {
                let opened: Bool = try await core.request("sona_open_link", ["link": link])
                error = opened ? nil : "That link doesn't point at anything any more."
            } catch {
                report(error)
            }
        }
    }

    /// Turns on the one switch that lets an Ask turn carry quotes.
    func allowRemoteIntelligence() {
        guard !busy else { return }
        busy = true
        error = nil
        Task {
            defer { busy = false }
            do {
                try await core.request(
                    "change_meeting_remote_intelligence_enabled_setting", ["enabled": true])
                await readSettings()
            } catch {
                report(error)
            }
        }
    }

    // MARK: - Plumbing

    /// One shape for every command that answers with a status.
    private func run(_ work: () async throws -> AgentPanelStatus) async -> Bool {
        busy = true
        error = nil
        defer { busy = false }
        do {
            hold(try await work())
            return true
        } catch {
            report(error)
            return false
        }
    }

    private func hold(_ next: AgentPanelStatus) {
        let switched = next.conversationId != status?.conversationId
        status = next
        tick()
        if switched {
            // Another window can start a conversation; the menu follows it.
            Task { await loadHistory() }
        }
    }

    private func report(_ failure: Error) {
        if let refusal = (failure as? CoreError)?.remote(as: AgentPanelCommandError.self) {
            error = refusal.message
        } else if let message = (failure as? CoreError)?.remote(as: String.self) {
            error = message
        } else {
            error = failure.localizedDescription
        }
    }

    /// Runs the elapsed clock while a turn runs, and only then.
    private func tick() {
        now = Self.milliseconds()
        guard running else {
            clock?.cancel()
            clock = nil
            return
        }
        guard clock == nil else { return }
        clock = Task { [weak self] in await self?.advance() }
    }

    private func advance() async {
        while running {
            try? await Task.sleep(for: .seconds(1))
            /* A cancelled clock has already been replaced or cleared by
             * `tick`, so it must not touch either field on its way out. */
            if Task.isCancelled { return }
            now = Self.milliseconds()
        }
        clock = nil
    }

    static func milliseconds() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }

    // MARK: - Request bodies

    /// Every panel command takes its arguments under one `request` key, and
    /// the fields inside it keep the wire's own names.
    private struct Envelope<Request: Encodable>: Encodable {
        let request: Request
    }

    private struct SendTurnRequest: Encodable {
        let turnId: String
        let message: String
        let locale: String
        let workspace: String
        let contextPack: String?
        let toolsAllowed: Bool

        enum CodingKeys: String, CodingKey {
            case turnId = "turn_id"
            case message
            case locale
            case workspace
            case contextPack = "context_pack"
            case toolsAllowed = "tools_allowed"
        }
    }

    private struct TurnRequest: Encodable {
        let turnId: String

        enum CodingKeys: String, CodingKey {
            case turnId = "turn_id"
        }
    }

    private struct ActionRequest: Encodable {
        let turnId: String
        let actionIndex: UInt32

        enum CodingKeys: String, CodingKey {
            case turnId = "turn_id"
            case actionIndex = "action_index"
        }
    }

    private struct ApplyChangeRequest: Encodable {
        let proposalId: String
        let expectedRevision: UInt64
        /// The card was on screen and the press was the confirmation.
        let confirmed: Bool

        enum CodingKeys: String, CodingKey {
            case proposalId = "proposal_id"
            case expectedRevision = "expected_revision"
            case confirmed
        }
    }

    private struct UndoChangeRequest: Encodable {
        let receiptId: String
        let expectedRevision: UInt64

        enum CodingKeys: String, CodingKey {
            case receiptId = "receipt_id"
            case expectedRevision = "expected_revision"
        }
    }
}

/// The pairing section's state: the switch, this Mac's public identity, and
/// the relay the panel is allowed to talk to.
///
/// The saved pairing lives in settings; the three fields here are a draft of
/// it, and they are re-seeded whenever the saved one changes underneath.
@MainActor
@Observable
final class AgentPairingStore {
    private(set) var settings: ChatSettings?
    /// This Mac's public half. The private half never leaves the keychain.
    private(set) var identity: AgentPanelPublicIdentity?
    var relayUrl = ""
    var relayKeyId = ""
    var relayPublicKey = ""
    private(set) var busy = false
    private(set) var error: String?
    /// The last test reached the relay and it answered. Cleared by the next
    /// thing the reader does, because it is a fact about one moment.
    private(set) var reached = false
    /// The identity is on the clipboard.
    private(set) var copied = false

    @ObservationIgnored private let core: Core
    /// The saved pairing as last seen, so an edit in progress is not thrown
    /// away by a settings event that changed something else.
    @ObservationIgnored private var saved: [String] = []
    @ObservationIgnored private var copyReset: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.chatSettingsChanged) { [weak self] _ in
            Task { await self?.readSettings() }
        }
    }

    func start() async {
        await readSettings()
        await readIdentity()
    }

    var isEnabled: Bool { settings?.isEnabled ?? true }
    var isPaired: Bool { settings?.isPaired ?? false }
    var lastTested: Date? { settings?.lastTested }

    /// Nothing can be asked of the panel before its settings have been read.
    var isBusy: Bool { busy || settings == nil }

    /// A pairing needs all three parts: an address, the key that signs its
    /// answers, and the id that names the key.
    var isComplete: Bool {
        ![relayUrl, relayKeyId, relayPublicKey].contains {
            $0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    var hasEdits: Bool { [relayUrl, relayKeyId, relayPublicKey] != saved }
    var canSave: Bool { !isBusy && isComplete && hasEdits }
    /* Saving and testing stay separate: a relay that is asleep is still the
     * relay you paired with, so Test asks about the saved pairing and says
     * nothing about an edit that has not been saved yet. */
    var canTest: Bool { !isBusy && isPaired }
    var canClear: Bool { !isBusy && isPaired }

    // MARK: - Commands

    /// The switch. Turning the panel on is also what creates this Mac's key,
    /// so the identity is read again after it moves.
    func setEnabled(_ enabled: Bool) {
        guard !isBusy else { return }
        busy = true
        error = nil
        reached = false
        Task {
            defer { busy = false }
            do {
                try await core.request("change_agent_panel_enabled_setting", ["enabled": enabled])
                await readSettings()
                await readIdentity()
            } catch {
                report(error)
            }
        }
    }

    /// Saves the three fields as the pairing. The relay is not contacted:
    /// that is what Test is for.
    func save() {
        guard canSave else { return }
        let pairing = PairingRequest(
            relayUrl: relayUrl.trimmingCharacters(in: .whitespacesAndNewlines),
            relayKeyId: relayKeyId.trimmingCharacters(in: .whitespacesAndNewlines),
            relayPublicKey: relayPublicKey.trimmingCharacters(in: .whitespacesAndNewlines))
        Task {
            _ = await run {
                try await self.core.request(
                    "set_agent_panel_pairing", PairingEnvelope(request: pairing))
            }
        }
    }

    /// Forgets the relay. Turns already sent are not recalled; nothing new
    /// can leave until something is paired again.
    func clear() {
        guard canClear else { return }
        Task {
            _ = await run { try await self.core.request("clear_agent_panel_pairing") }
        }
    }

    /// One signed round trip to the relay, which is the only thing that
    /// stamps the last-tested time.
    func test() {
        guard canTest else { return }
        Task {
            let receipt = await run {
                try await self.core.request("agent_panel_test_connection")
            }
            /* A receipt is the answer: the relay signed something and this
             * Mac believed it. */
            reached = receipt != nil
        }
    }

    /// Puts the public half on the clipboard, because it belongs in the
    /// relay's own allowlist and nobody should retype a key by hand.
    func copyIdentity() {
        guard let identity else { return }
        let board = NSPasteboard.general
        board.clearContents()
        board.setString(identity.publicKey, forType: .string)
        copied = true
        copyReset?.cancel()
        copyReset = Task { [weak self] in
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled else { return }
            self?.copied = false
        }
    }

    // MARK: - Plumbing

    private func readSettings() async {
        do {
            hold(try await core.request("get_app_settings"))
        } catch {
            report(error)
        }
    }

    private func readIdentity() async {
        do {
            identity = try await core.request("agent_panel_public_identity")
        } catch {
            /* The panel mints the key when it is switched on, so a missing
             * identity is the expected state while it is off rather than a
             * failure worth a red line. */
            identity = nil
        }
    }

    private func hold(_ next: ChatSettings) {
        let pairing = [next.relayUrl, next.relayKeyId, next.relayPublicKey]
        if pairing != saved {
            saved = pairing
            relayUrl = next.relayUrl
            relayKeyId = next.relayKeyId
            relayPublicKey = next.relayPublicKey
        }
        settings = next
    }

    /// One shape for the three commands that answer with a receipt.
    private func run(_ work: () async throws -> AgentPairingReceipt) async -> AgentPairingReceipt? {
        busy = true
        error = nil
        reached = false
        defer { busy = false }
        do {
            let receipt = try await work()
            /* The receipt carries the committed pairing, but the saved
             * settings are what every other screen reads, so they are what
             * the fields are re-seeded from. */
            await readSettings()
            return receipt
        } catch {
            report(error)
            return nil
        }
    }

    private func report(_ failure: Error) {
        if let refusal = (failure as? CoreError)?.remote(as: AgentPanelCommandError.self) {
            error = refusal.message
        } else if let message = (failure as? CoreError)?.remote(as: String.self) {
            error = message
        } else {
            error = failure.localizedDescription
        }
    }

    private struct PairingEnvelope: Encodable {
        let request: PairingRequest
    }

    private struct PairingRequest: Encodable {
        let relayUrl: String
        let relayKeyId: String
        let relayPublicKey: String

        enum CodingKeys: String, CodingKey {
            case relayUrl = "relay_url"
            case relayKeyId = "relay_key_id"
            case relayPublicKey = "relay_public_key"
        }
    }
}

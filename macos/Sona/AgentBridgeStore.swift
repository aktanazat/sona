import AppKit
import Foundation

struct AgentBridgeQuestionAnswerDraft {
    var selected: Set<String> = []
    var custom = ""
}

struct AgentBridgeQuestionDraft {
    let questions: [AgentBridgeQuestion]
    var answers: [AgentBridgeQuestionAnswerDraft]

    init(questions: [AgentBridgeQuestion]) {
        self.questions = questions
        answers = Array(repeating: AgentBridgeQuestionAnswerDraft(), count: questions.count)
    }

    var canSend: Bool {
        !questions.isEmpty && zip(questions, answers).allSatisfy { question, answer in
            let custom = !answer.custom.isEmpty && !answer.selected.contains(answer.custom)
            let count = answer.selected.count + (custom ? 1 : 0)
            return (!question.required || count > 0) && (question.multiSelect || count <= 1)
                && (!custom || (question.custom && answer.custom.contains { !$0.isWhitespace }))
        }
    }

    var values: [[String]] {
        zip(questions, answers).map { question, answer in
            var values = question.options.compactMap { answer.selected.contains($0.label) ? $0.label : nil }
            if !answer.custom.isEmpty && !answer.selected.contains(answer.custom) {
                values.append(answer.custom)
            }
            return values
        }
    }

    var text: String {
        var text = ""
        for (question, answer) in zip(questions, answers) {
            text += question.question + "\n"
            var separator = ""
            for option in question.options where answer.selected.contains(option.label) {
                text += separator + option.label
                separator = ", "
            }
            if !answer.custom.isEmpty && !answer.selected.contains(answer.custom) {
                text += separator + answer.custom
            }
            text += "\n\n"
        }
        return text
    }
}

/// The coding-agent bridge's operator console: what the bridge may do, what it
/// has been doing, and the replies waiting to be sent.
///
/// One read fills the whole console — status, sessions, requests and the
/// pending queue come back together — and the core's own update event replays
/// that read, so a move an agent makes on its own arrives without anyone
/// asking. Every write reports refusal as a command error, so each action
/// funnels its failure into `error` and then re-reads what the write moved.
@MainActor
@Observable
final class AgentBridgeStore {
    /// The persisted policy: the switches, the authorized projects, the rules.
    private(set) var bridge = AgentBridgeSettings.empty
    private(set) var status: AgentBridgeStatus?
    private(set) var sessions: [AgentBridgeObservedSession] = []
    private(set) var requests: [AgentBridgeObservedRequest] = []
    private(set) var pending: [AgentBridgePendingMessage] = []
    /// The agent configuration to paste, and why it could not be produced.
    private(set) var hookSnippets: [AgentBridgeAgent: String] = [:]
    var hookAgent = AgentBridgeAgent.claude {
        didSet { hookCopied = false }
    }
    var hookSnippet: String? { hookSnippets[hookAgent] }
    private(set) var openCodeInstalled = false
    private(set) var hookError: String?
    private(set) var hookCopied = false
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    private(set) var loading = false
    private(set) var authorizing = false
    /// Whether an agent's edits to tasks, and what Sona fills in from a
    /// meeting, land at once or wait for the user.
    private(set) var agentChanges = WorkAutonomy.suggest
    private(set) var sonaChanges = WorkAutonomy.suggest
    /// Whether a task closes itself once its sub-tasks are done.
    private(set) var closeParents = false
    /// Whether closing a parent closes its open sub-tasks.
    private(set) var closeSubtasks = false
    /// Whether starting a sub-task moves its parent to in progress.
    private(set) var startParent = false
    /// Whether the weekly project digest reaches the inbox, and on which day.
    private(set) var digestEnabled = true
    private(set) var digestDay = DigestDay.monday
    /// Days before stale tasks auto-close or completed work auto-archives; nil = off.
    private(set) var autoCloseStaleDays: Int?
    private(set) var autoArchiveDays: Int?
    /// The fields Triage fills by itself when every similar task agrees.
    private(set) var triageAutoApply: Set<WorkAutoField> = []
    /// Whether the last settings read failed, so the values above are
    /// defaults rather than what the file holds.
    private(set) var settingsUnread = false
    private(set) var autonomyBusy = false
    /// The tasks a session waits on the user's approval for, by the name the
    /// session gave each. Set only when it changes, so task lists redraw then
    /// and not on every hook call.
    private var waitingTasks: Set<String> = []
    /// The composer: which session a reply is for, and its words.
    var replySessionId = ""
    var replyText = ""
    /// Outside MCP servers Talk to Sona may call read-only.
    private(set) var mcpServers: [AgentMcpServerSummary] = []
    private(set) var mcpTools: [String: [AgentMcpToolSummary]] = [:]
    private(set) var mcpToolsLoading: Set<String> = []
    private(set) var mcpBusy = false
    private(set) var mcpError: String?
    private(set) var questionDrafts: [String: AgentBridgeQuestionDraft] = [:]
    private(set) var respondingRequests: Set<String> = []

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.agentBridgeUpdate) { [weak self] line in self?.apply(line) }
        core.observe(CoreEvent.agentBridgeSettingsChanged) { [weak self] _ in
            guard let self else { return }
            Task { await self.loadSettings() }
        }
    }

    /// Replies can only be written while the bridge is on and live; every
    /// control that would write one is off until then.
    var interactiveReady: Bool { bridge.masterEnabled && status?.diagnostic == .active }

    /// The sessions a reply can actually reach.
    var replySessions: [AgentBridgeObservedSession] { sessions.filter(\.acceptsReply) }

    /// Whether an agent session working on this task waits on the user's
    /// approval. Sessions name a task "S-12" or by its id.
    func waits(on row: WorkItemRow) -> Bool {
        waitingTasks.contains(row.id) || waitingTasks.contains("S-\(row.number)")
    }

    var canCreatePreview: Bool {
        interactiveReady && !replySessionId.isEmpty
            && !replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    func start() async {
        await refresh()
        await loadSettings()
        await loadHookSnippet()
        await loadMcpServers()
    }

    /// The Refresh button: the observations, the policy, and one more try at
    /// the setup code, which is the only way back from a failed first read.
    func refreshAll() {
        Task {
            await refresh()
            await loadSettings()
            await loadHookSnippet()
            await loadMcpServers()
        }
    }

    func loadMcpServers() async {
        do {
            mcpServers = try await core.request("agent_mcp_servers_list")
            mcpError = nil
        } catch {
            mcpError = Self.reason(error)
        }
    }

    func addMcpServer(name: String, url: String, token: String) async -> Bool {
        mcpBusy = true
        mcpError = nil
        defer { mcpBusy = false }
        let trimmedToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
        do {
            mcpServers = try await core.request(
                "agent_mcp_server_add",
                [
                    "name": JSONValue.string(name),
                    "url": .string(url),
                    "token": trimmedToken.isEmpty ? .null : .string(trimmedToken),
                ])
            return true
        } catch {
            mcpError = Self.reason(error)
            return false
        }
    }

    func removeMcpServer(_ id: String) {
        mcpBusy = true
        mcpError = nil
        Task {
            defer { mcpBusy = false }
            do {
                mcpServers = try await core.request("agent_mcp_server_remove", ["id": JSONValue.string(id)])
                mcpTools.removeValue(forKey: id)
            } catch {
                mcpError = Self.reason(error)
            }
        }
    }

    func inspectMcpTools(_ id: String) {
        guard !mcpToolsLoading.contains(id) else { return }
        mcpToolsLoading.insert(id)
        mcpError = nil
        Task {
            defer { mcpToolsLoading.remove(id) }
            do {
                let tools: [AgentMcpToolSummary] = try await core.request(
                    "agent_mcp_server_tools", ["id": JSONValue.string(id)])
                mcpTools[id] = tools
            } catch {
                mcpError = Self.reason(error)
            }
        }
    }

    func setMcpTool(_ id: String, tool: String, enabled: Bool) {
        mcpError = nil
        Task {
            do {
                try await core.request(
                    "agent_mcp_server_tool_set",
                    ["id": JSONValue.string(id), "tool": .string(tool), "enabled": .bool(enabled)])
                if let index = mcpTools[id]?.firstIndex(where: { $0.name == tool }) {
                    mcpTools[id]?[index].enabled = enabled
                }
            } catch {
                mcpError = Self.reason(error)
            }
        }
    }

    /// Status, sessions, requests and the queue, in one round trip.
    func refresh() async {
        loading = true
        defer { loading = false }
        do {
            async let status: AgentBridgeStatus = core.request("get_agent_bridge_status")
            async let sessions: [AgentBridgeObservedSession] = core.request("get_agent_bridge_sessions")
            async let requests: [AgentBridgeObservedRequest] = core.request("get_agent_bridge_requests")
            async let pending: [AgentBridgePendingMessage] = core.request("get_agent_bridge_pending_messages")
            self.status = try await status
            self.sessions = try await sessions
            self.requests = try await requests
            self.pending = try await pending
            for request in self.requests where request.state == .delivered {
                questionDrafts.removeValue(forKey: request.id)
            }
            alignReplySession()
            alignWaiting()
            error = nil
        } catch {
            self.error = "Couldn't load agent connection details: \(Self.reason(error))"
        }
    }

    func setMaster(_ enabled: Bool) {
        mutate {
            try await self.core.request("set_agent_bridge_master", ["enabled": JSONValue.bool(enabled)])
        }
    }

    func setAgent(_ agent: AgentBridgeAgent, enabled: Bool) {
        mutate {
            try await self.core.request(
                "set_agent_bridge_agent_enabled",
                ["agent": JSONValue.string(agent.rawValue), "enabled": .bool(enabled)])
        }
    }

    func setSessionVoice(_ sessionId: String, enabled: Bool) {
        act {
            try await self.core.request(
                "set_agent_bridge_session_voice",
                ["sessionId": JSONValue.string(sessionId), "enabled": .bool(enabled)])
        }
    }

    /// The folder picker the webview reached through a Tauri plugin. Only the
    /// path leaves this window: the core hashes it and keeps the hash. The
    /// panel runs without blocking the run loop, so the button stays busy
    /// from the moment it opens until the write comes back.
    func authorizeProject() {
        guard !authorizing else { return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Choose a project folder agents may work in."
        panel.prompt = "Authorize"
        authorizing = true
        panel.begin { [weak self] response in
            // AppKit calls this on the main thread, where the store lives.
            MainActor.assumeIsolated {
                guard let store = self else { return }
                guard response == .OK, let folder = panel.url else {
                    store.authorizing = false
                    return
                }
                store.authorize(folder)
            }
        }
    }

    private func authorize(_ folder: URL) {
        Task {
            do {
                bridge = try await core.request(
                    "authorize_agent_bridge_project",
                    ["selectedPath": JSONValue.string(folder.path(percentEncoded: false))])
                error = nil
                await refresh()
            } catch {
                self.error = Self.failure(error)
            }
            authorizing = false
        }
    }

    func removeProject(_ canonicalProjectHash: String) {
        mutate {
            try await self.core.request(
                "remove_agent_bridge_project",
                ["canonicalProjectHash": JSONValue.string(canonicalProjectHash)])
        }
    }

    /// Step one of the two-step reply: the core holds the exact words, and
    /// nothing is written to the agent until they are confirmed.
    func createReplyPreview() {
        guard canCreatePreview else { return }
        let session = replySessionId
        let text = replyText
        act {
            let _: AgentBridgePendingMessage = try await self.core.request(
                "create_agent_bridge_reply_preview",
                ["sessionId": JSONValue.string(session), "text": .string(text)])
            self.replyText = ""
        }
    }

    /// Step two: the same id, session and words the preview came back with.
    /// The core refuses anything else.
    func confirmPending(_ message: AgentBridgePendingMessage) {
        act {
            let _: AgentBridgePendingMessage = try await self.core.request(
                "confirm_agent_bridge_reply",
                [
                    "pendingId": JSONValue.string(message.id),
                    "sessionId": .string(message.sessionId),
                    "text": .string(message.text),
                ])
        }
    }

    func cancelPending(_ pendingId: String) {
        act {
            try await self.core.request(
                "cancel_agent_bridge_message", ["pendingId": JSONValue.string(pendingId)])
        }
    }

    func dismissRequest(_ requestId: String) {
        act {
            try await self.core.request(
                "dismiss_agent_bridge_request", ["requestId": JSONValue.string(requestId)])
        }
    }

    /// OpenCode approves only this request. Legacy agents still require an
    /// exact saved rule before the hook may accept their answer.
    func decidePermission(
        _ request: AgentBridgeObservedRequest, _ decision: AgentBridgePermissionDecision
    ) {
        if request.agent == .opencode {
            act(requestId: request.id) {
                try await self.core.request(
                    "respond_agent_bridge_permission",
                    ["requestId": JSONValue.string(request.id), "ruleId": .null,
                     "decision": .string(decision.rawValue)])
            }
            return
        }
        Task {
            do {
                let rule: AgentBridgePermissionRule = try await core.request(
                    "create_agent_bridge_permission_rule",
                    ["requestId": JSONValue.string(request.id), "decision": .string(decision.rawValue)])
                var refused: String?
                do {
                    try await core.request(
                        "respond_agent_bridge_permission",
                        [
                            "requestId": JSONValue.string(request.id),
                            "ruleId": .string(rule.id),
                            "decision": .string(decision.rawValue),
                        ])
                } catch {
                    refused = Self.failure(error)
                }
                await loadSettings()
                await refresh()
                // After the re-reads, so a refusal is still the last word.
                if let refused {
                    error = refused
                }
            } catch {
                self.error = Self.failure(error)
            }
        }
    }

    func setQuestionOption(
        _ request: AgentBridgeObservedRequest, question: Int, label: String, selected: Bool
    ) {
        prepareQuestionDraft(request)
        if selected && request.questions?[question].multiSelect == false {
            questionDrafts[request.id]?.answers[question].selected.removeAll(keepingCapacity: true)
            questionDrafts[request.id]?.answers[question].custom = ""
        }
        if selected {
            questionDrafts[request.id]?.answers[question].selected.insert(label)
        } else {
            questionDrafts[request.id]?.answers[question].selected.remove(label)
        }
    }

    func setQuestionCustomAnswer(_ request: AgentBridgeObservedRequest, question: Int, text: String) {
        prepareQuestionDraft(request)
        questionDrafts[request.id]?.answers[question].custom = text
        if !text.isEmpty && request.questions?[question].multiSelect == false {
            questionDrafts[request.id]?.answers[question].selected.removeAll(keepingCapacity: true)
        }
    }

    func sendQuestionAnswers(_ request: AgentBridgeObservedRequest) {
        guard let questions = request.questions else { return }
        let draft = questionDrafts[request.id] ?? AgentBridgeQuestionDraft(questions: questions)
        guard draft.canSend else { return }
        respondQuestions(request, response: .object([
            "decision": .string("answer"),
            "answers": .array(draft.values.map { .array($0.map(JSONValue.string)) }),
        ]))
    }

    func declineQuestions(_ request: AgentBridgeObservedRequest) {
        respondQuestions(request, response: .object(["decision": .string("reject")]))
    }

    func copyQuestionAnswers(_ requestId: String) {
        guard let draft = questionDrafts[requestId] else { return }
        let board = NSPasteboard.general
        board.clearContents()
        if !board.setString(draft.text, forType: .string) {
            error = "The answers could not be copied."
        }
    }

    private func prepareQuestionDraft(_ request: AgentBridgeObservedRequest) {
        if questionDrafts[request.id] == nil, let questions = request.questions {
            questionDrafts[request.id] = AgentBridgeQuestionDraft(questions: questions)
        }
    }

    private func respondQuestions(_ request: AgentBridgeObservedRequest, response: JSONValue) {
        act(requestId: request.id) {
            try await self.core.request(
                "respond_agent_bridge_questions",
                ["requestId": JSONValue.string(request.id), "response": response])
        }
    }

    func deleteRule(_ ruleId: String) {
        mutate {
            try await self.core.request(
                "delete_agent_bridge_permission_rule", ["ruleId": JSONValue.string(ruleId)])
        }
    }

    func setAgentChanges(_ autonomy: WorkAutonomy) async {
        await setAutonomy("change_work_agent_changes_setting", autonomy, keyPath: \.agentChanges)
    }

    func setSonaChanges(_ autonomy: WorkAutonomy) async {
        await setAutonomy("change_work_sona_changes_setting", autonomy, keyPath: \.sonaChanges)
    }

    func setCloseParents(_ enabled: Bool) async {
        await setWorkFlag("change_work_close_parents_setting", enabled, keyPath: \.closeParents)
    }

    func setCloseSubtasks(_ enabled: Bool) async {
        await setWorkFlag("change_work_close_subtasks_setting", enabled, keyPath: \.closeSubtasks)
    }

    func setStartParent(_ enabled: Bool) async {
        await setWorkFlag("change_work_start_parent_setting", enabled, keyPath: \.startParent)
    }

    private func setWorkFlag(
        _ method: String, _ enabled: Bool, keyPath: ReferenceWritableKeyPath<AgentBridgeStore, Bool>
    ) async {
        autonomyBusy = true
        defer { autonomyBusy = false }
        let previous = self[keyPath: keyPath]
        self[keyPath: keyPath] = enabled
        do {
            try await core.request(method, ["enabled": enabled])
            error = nil
            await loadSettings()
        } catch {
            self[keyPath: keyPath] = previous
            self.error = "Couldn't save that setting: \(Self.reason(error))"
        }
    }

    /// Throws so the digest's own section can say what failed beside its
    /// controls.
    func setDigest(_ enabled: Bool, _ day: DigestDay) async throws {
        let previous = (digestEnabled, digestDay)
        (digestEnabled, digestDay) = (enabled, day)
        do {
            try await core.request(
                "change_work_digest_setting", ["enabled": JSONValue.bool(enabled), "day": .string(day.rawValue)])
        } catch {
            (digestEnabled, digestDay) = previous
            throw error
        }
        await loadSettings()
    }

    func setTriageAutoApply(_ field: WorkAutoField, _ on: Bool) async {
        autonomyBusy = true
        defer { autonomyBusy = false }
        let previous = triageAutoApply
        if on { triageAutoApply.insert(field) } else { triageAutoApply.remove(field) }
        do {
            try await core.request(
                "change_work_triage_auto_apply_setting",
                ["fields": JSONValue.array(WorkAutoField.allCases.filter(triageAutoApply.contains).map { .string($0.rawValue) })])
            error = nil
            await loadSettings()
        } catch {
            triageAutoApply = previous
            self.error = "Couldn't save that setting: \(Self.reason(error))"
        }
    }

    func setHousekeeping(autoCloseStaleDays: Int?, autoArchiveDays: Int?) async throws {
        let previous = (self.autoCloseStaleDays, self.autoArchiveDays)
        (self.autoCloseStaleDays, self.autoArchiveDays) = (autoCloseStaleDays, autoArchiveDays)
        do {
            try await core.request(
                "change_work_housekeeping_setting",
                [
                    "autoCloseStaleDays": autoCloseStaleDays.map { JSONValue.number(Double($0)) } ?? .null,
                    "autoArchiveDays": autoArchiveDays.map { JSONValue.number(Double($0)) } ?? .null,
                ])
        } catch {
            (self.autoCloseStaleDays, self.autoArchiveDays) = previous
            throw error
        }
        await loadSettings()
    }

    private func setAutonomy(
        _ method: String, _ autonomy: WorkAutonomy, keyPath: ReferenceWritableKeyPath<AgentBridgeStore, WorkAutonomy>
    ) async {
        autonomyBusy = true
        defer { autonomyBusy = false }
        let previous = self[keyPath: keyPath]
        self[keyPath: keyPath] = autonomy
        do {
            try await core.request(method, ["autonomy": autonomy])
            error = nil
            await loadSettings()
        } catch {
            self[keyPath: keyPath] = previous
            self.error = "Couldn't save that setting: \(Self.reason(error))"
        }
    }

    /// The clipboard the webview reached through `navigator.clipboard`.
    func copyHookSnippet() {
        guard let snippet = hookSnippet else { return }
        let board = NSPasteboard.general
        board.clearContents()
        guard board.setString(snippet, forType: .string) else {
            hookError = "The setup code could not be copied."
            return
        }
        hookError = nil
        hookCopied = true
    }

    /// The bridge policy and work subset of the app's settings.
    /// `settings-changed` carries nothing, so the whole read happens again.
    private func loadSettings() async {
        do {
            let settings: AgentBridgeAppSettings = try await core.request("get_app_settings")
            if let bridge = settings.agentBridge {
                self.bridge = bridge
            }
            agentChanges = settings.workAgentChanges ?? .suggest
            sonaChanges = settings.workSonaChanges ?? .suggest
            closeParents = settings.workCloseParents ?? false
            closeSubtasks = settings.workCloseSubtasks ?? false
            startParent = settings.workStartParent ?? false
            digestEnabled = settings.workDigestEnabled ?? true
            digestDay = settings.workDigestDay ?? .monday
            autoCloseStaleDays = settings.workAutoCloseStaleDays
            autoArchiveDays = settings.workAutoArchiveDays
            triageAutoApply = Set(settings.workTriageAutoApply ?? [])
            error = nil
            settingsUnread = false
        } catch {
            settingsUnread = true
            self.error = "Couldn't load agent connection details: \(Self.reason(error))"
        }
    }

    /// The agent configuration to paste. A failure replaces the snippet
    /// rather than sitting above it: an unavailable setup code and a setup
    /// code on the same surface contradict each other.
    private func loadHookSnippet() async {
        openCodeInstalled = Self.detectOpenCode()
        hookCopied = false
        do {
            let raw: String = try await core.request("get_agent_bridge_hook_snippet")
            let snippets = try JSONDecoder().decode([String: JSONValue].self, from: Data(raw.utf8))
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
            var available: [AgentBridgeAgent: String] = [:]
            for agent in AgentBridgeAgent.allCases {
                if let snippet = snippets[agent.rawValue] {
                    available[agent] = String(decoding: try encoder.encode(snippet), as: UTF8.self)
                }
            }
            hookSnippets = available
            hookError = nil
        } catch {
            hookSnippets.removeAll(keepingCapacity: true)
            hookError = Self.reason(error)
        }
    }

    private static func detectOpenCode() -> Bool {
        let files = FileManager.default
        func executable(_ path: String) -> Bool {
            var directory = ObjCBool(false)
            return files.fileExists(atPath: path, isDirectory: &directory)
                && !directory.boolValue && files.isExecutableFile(atPath: path)
        }
        if let path = ProcessInfo.processInfo.environment["PATH"] {
            for directory in path.split(separator: ":") {
                if executable(String(directory) + "/opencode") { return true }
            }
        }
        return executable(files.homeDirectoryForCurrentUser.appending(path: ".opencode/bin/opencode").path)
            || executable("/opt/homebrew/bin/opencode")
            || executable("/usr/local/bin/opencode")
    }

    private func apply(_ line: Data) {
        if let update: AgentBridgeUpdate = try? Core.payload(line) {
            status = update.status
        }
        Task { await refresh() }
    }

    /// A write that answers with the whole policy.
    private func mutate(_ work: @escaping @MainActor () async throws -> AgentBridgeSettings) {
        Task {
            do {
                bridge = try await work()
                error = nil
                await refresh()
            } catch {
                self.error = Self.failure(error)
            }
        }
    }

    /// A write that answers with nothing the console keeps.
    private func act(requestId: String? = nil, _ work: @escaping @MainActor () async throws -> Void) {
        if let requestId, !respondingRequests.insert(requestId).inserted { return }
        Task {
            defer {
                if let requestId { respondingRequests.remove(requestId) }
            }
            do {
                try await work()
                error = nil
                await refresh()
            } catch {
                self.error = Self.failure(error)
            }
        }
    }

    /// A session that went away takes the composer's destination with it.
    private func alignReplySession() {
        if !replySessions.contains(where: { $0.id == replySessionId }) {
            replySessionId = replySessions.first?.id ?? ""
        }
    }

    /// A task waits while a permission its session asked for still takes an
    /// answer.
    private func alignWaiting() {
        let asking = Set(requests.filter { $0.isOpen && $0.awaitingResponse && $0.kind.isPermission }.map(\.sessionId))
        let waiting = Set(sessions.filter { asking.contains($0.id) }.compactMap(\.task))
        if waiting != waitingTasks { waitingTasks = waiting }
    }

    private static func failure(_ error: Error) -> String {
        "Couldn't finish that agent action: \(reason(error))"
    }

    /// The bridge names its refusals after its own error enum, which reads
    /// like `DuplicatePending`. Anything else the core says is passed through
    /// as it came.
    private static func reason(_ error: Error) -> String {
        let message = error.localizedDescription
        return refusals[message] ?? message
    }

    private static let refusals: [String: String] = [
        "Disabled": "agent connections are off",
        "VoiceDisabled": "voice replies are off for that conversation",
        "RuntimeUnavailable": "the bridge is not running",
        "InteractiveUnsupported": "replies aren't available",
        "AppLockHeld": "another Sona instance is active",
        "UnknownSession": "that session is no longer there",
        "UnknownRequest": "that request is no longer there",
        "WrongAgent": "that belongs to a different agent",
        "WrongDestination": "that reply is for a different session",
        "Expired": "it expired",
        "DuplicatePending": "that session already has a reply waiting",
        "EmptyMessage": "the reply is empty",
        "ConfirmationMismatch": "the words changed since the preview",
        "RuleRequired": "a permission rule has to exist first",
        "RuleMismatch": "the rule does not match this request",
        "PermissionResponseUnsupported": "nothing is waiting for an answer",
        "InvalidAnswers": "answer each required question and choose only the allowed number of answers",
        "AlreadyHandled": "it was already handled",
        "Unauthorized": "that project or agent is no longer authorized",
        "PersistenceFailed": "the change could not be saved",
    ]
}

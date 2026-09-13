import AppKit
import Foundation

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
    private(set) var hookSnippet: String?
    private(set) var hookError: String?
    private(set) var hookCopied = false
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    private(set) var loading = false
    private(set) var authorizing = false
    /// The composer: which session a reply is for, and its words.
    var replySessionId = ""
    var replyText = ""

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

    var canCreatePreview: Bool {
        interactiveReady && !replySessionId.isEmpty
            && !replyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    func start() async {
        await refresh()
        await loadSettings()
        await loadHookSnippet()
    }

    /// The Refresh button: the observations, the policy, and one more try at
    /// the setup code, which is the only way back from a failed first read.
    func refreshAll() {
        Task {
            await refresh()
            await loadSettings()
            await loadHookSnippet()
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
            alignReplySession()
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

    /// Answering a request is two writes: the exact rule that authorizes the
    /// answer, then the answer itself. The rule is saved even when the answer
    /// fails to reach the agent, so the policy is re-read either way.
    func decidePermission(
        _ request: AgentBridgeObservedRequest, _ decision: AgentBridgePermissionDecision
    ) {
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

    func deleteRule(_ ruleId: String) {
        mutate {
            try await self.core.request(
                "delete_agent_bridge_permission_rule", ["ruleId": JSONValue.string(ruleId)])
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
        Task {
            try? await Task.sleep(for: .seconds(2))
            hookCopied = false
        }
    }

    /// The bridge policy subset of the app's settings. `settings-changed`
    /// carries nothing, so the whole read happens again.
    private func loadSettings() async {
        do {
            let settings: AgentBridgeAppSettings = try await core.request("get_app_settings")
            if let bridge = settings.agentBridge {
                self.bridge = bridge
            }
            error = nil
        } catch {
            self.error = "Couldn't load agent connection details: \(Self.reason(error))"
        }
    }

    /// The agent configuration to paste. A failure replaces the snippet
    /// rather than sitting above it: an unavailable setup code and a setup
    /// code on the same surface contradict each other.
    private func loadHookSnippet() async {
        do {
            hookSnippet = try await core.request("get_agent_bridge_hook_snippet") as String
            hookError = nil
        } catch {
            hookSnippet = nil
            hookError = Self.reason(error)
        }
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
    private func act(_ work: @escaping @MainActor () async throws -> Void) {
        Task {
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
        "AlreadyHandled": "it was already handled",
        "PersistenceFailed": "the change could not be saved",
    ]
}

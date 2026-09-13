import Foundation

/// The coding-agent bridge's shapes, as `src-tauri/src/agent_bridge.rs` and
/// `src-tauri/src/settings.rs` serialize them. Snake_case arrives as camelCase
/// through `Core.decoder`; enum values are the wire strings themselves.

extension CoreEvent {
    /// The bridge's own heartbeat: `{"status": AgentBridgeStatus}`. The core
    /// sends it whenever the runtime, a session, a request or the queue moved.
    static let agentBridgeUpdate = "agent-bridge-update-event"
    /// Carries nothing useful, so the store re-reads `get_app_settings` and
    /// keeps the bridge policy subset.
    static let agentBridgeSettingsChanged = "settings-changed"
}

/// The four agents the local hook bridge knows. A closed set in Rust too:
/// settings cannot name a fifth one into existence.
enum AgentBridgeAgent: String, Decodable, CaseIterable, Hashable, Sendable {
    case claude
    case codex
    case grok
    case omp

    var label: String {
        switch self {
        case .claude: "Claude"
        case .codex: "Codex"
        case .grok: "Grok"
        case .omp: "OMP"
        }
    }

    /// What Sona may do with this agent, which is the one thing the name
    /// cannot carry.
    var detail: String {
        switch self {
        case .claude: "Observe Claude sessions, and reply where Sona supports it."
        case .codex: "Observe Codex sessions, answer approval requests, and reply when a turn ends."
        case .grok: "Observe Grok sessions, answer tool requests, and reply when a turn ends."
        case .omp: "Observe OMP sessions and approval requests. Sona can reply only when an OMP session stops."
        }
    }
}

/// Why the bridge is or is not answering right now.
enum AgentBridgeDiagnostic: String, Decodable, Hashable, Sendable {
    case disabled
    case runtimeUnavailable = "runtime_unavailable"
    case interactiveUnsupported = "interactive_unsupported"
    case appLockHeld = "app_lock_held"
    case active

    var label: String {
        switch self {
        case .disabled: "Disabled"
        case .runtimeUnavailable: "Not running"
        case .interactiveUnsupported: "Replies aren't available"
        case .appLockHeld: "Another Sona instance is active"
        case .active: "Active"
        }
    }
}

struct AgentBridgeStatus: Decodable, Sendable {
    let running: Bool
    let diagnostic: AgentBridgeDiagnostic
    let policyGeneration: UInt64
    let observedSessions: Int
    let pendingMessages: Int
}

/// One agent session the hook has reported. The project is a hash: the bridge
/// never hands the shell a folder path.
struct AgentBridgeObservedSession: Decodable, Identifiable, Sendable {
    let id: String
    let agent: AgentBridgeAgent
    let canonicalProjectHash: String
    let sessionGeneration: UInt64
    let policyGeneration: UInt64
    let lastSeenAtMs: UInt64

    var lastSeen: Date { Date(timeIntervalSince1970: TimeInterval(lastSeenAtMs) / 1000) }

    /// The bridge writes a reply by continuing a stopped turn, and only these
    /// two agents have a channel for it.
    var acceptsReply: Bool { agent == .claude || agent == .omp }
}

enum AgentBridgeRequestKind: String, Decodable, Sendable {
    case sessionStart = "session_start"
    case userPromptSubmit = "user_prompt_submit"
    case permissionRequest = "permission_request"
    case preToolUse = "pre_tool_use"
    case postToolUse = "post_tool_use"
    case stop
    case notification

    var label: String {
        switch self {
        case .sessionStart: "Session started"
        case .userPromptSubmit: "Prompt submitted"
        case .permissionRequest: "Permission request"
        case .preToolUse: "Before tool use"
        case .postToolUse: "After tool use"
        case .stop: "Stop"
        case .notification: "Notification"
        }
    }

    /// The two kinds a permission rule can answer; the core refuses the rest.
    var isPermission: Bool { self == .permissionRequest || self == .preToolUse }
}

enum AgentBridgeRequestState: String, Decodable, Sendable {
    case observed
    case responded
    case dismissed
    case expired
}

/// One hook invocation the bridge saw, and whether it is still holding its
/// agent open for an answer.
struct AgentBridgeObservedRequest: Decodable, Identifiable, Sendable {
    let id: String
    let sessionId: String
    let agent: AgentBridgeAgent
    let kind: AgentBridgeRequestKind
    let toolName: String?
    let permissionMode: String?
    let expiresAtMs: UInt64
    let state: AgentBridgeRequestState
    /// The core's own answer to "can this be replied to", so the console and
    /// the responder never disagree about which invocations are waiting.
    let awaitingResponse: Bool

    var expiresAt: Date { Date(timeIntervalSince1970: TimeInterval(expiresAtMs) / 1000) }

    var isOpen: Bool { state == .observed }

    /// Allow and Deny appear only when an answer will actually reach the
    /// agent: the bridge is live, the row is open, the kind takes a rule, and
    /// the hook is still waiting.
    func canRespond(interactiveReady: Bool) -> Bool {
        interactiveReady && isOpen && kind.isPermission && awaitingResponse
    }

    /// The hook already returned, so Sona can only watch this one.
    var observeOnly: Bool { isOpen && kind.isPermission && !awaitingResponse }

    var headline: String {
        let head = "\(agent.label) · \(kind.label)"
        guard let toolName, !toolName.isEmpty else { return head }
        return "\(head) · \(toolName)"
    }
}

enum AgentBridgePendingState: String, Decodable, Sendable {
    case held
    case responseWritten = "response_written"
    case emitted
    case copyOnly = "copy_only"
    case cancelled

    var label: String {
        switch self {
        case .held: "Waiting until the agent stops"
        case .responseWritten: "Reply written"
        case .emitted: "Sent"
        case .copyOnly: "Copy-only"
        case .cancelled: "Cancelled"
        }
    }
}

/// A reply waiting on a confirmation. The text is held in the core's memory
/// only: confirming sends back the exact words that were previewed.
struct AgentBridgePendingMessage: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let agent: AgentBridgeAgent
    let sessionId: String
    let text: String
    let expiresAtMs: UInt64
    let state: AgentBridgePendingState
    let confirmed: Bool

    var expiresAt: Date { Date(timeIntervalSince1970: TimeInterval(expiresAtMs) / 1000) }

    var canConfirm: Bool { state == .held && !confirmed }

    var canCancel: Bool { state == .held || state == .copyOnly }

    /// What is true of this reply now, not what to do about it.
    var stateLabel: String {
        state == .held && confirmed ? "Will send when the agent stops" : state.label
    }
}

enum AgentBridgePermissionDecision: String, Decodable, Sendable {
    case allow
    case deny

    var label: String { self == .allow ? "Allow" : "Deny" }
}

/// One exact rule: this tool, with this input, in this project. The core
/// hashes the input itself, so a rule can never be widened from here.
struct AgentBridgePermissionRule: Decodable, Identifiable, Sendable {
    let id: String
    let agent: AgentBridgeAgent
    let canonicalProjectHash: String
    let toolName: String
    let permissionMode: String?
    let toolInputHash: String
    let decision: AgentBridgePermissionDecision
    let userCreated: Bool?
}

/// A project the agents may act in, as a hash of its canonical path.
struct AgentBridgeProjectScope: Decodable, Identifiable, Sendable {
    let canonicalProjectHash: String

    var id: String { canonicalProjectHash }
}

/// The persisted bridge policy: switches, authorized projects, exact rules.
/// No user text and no provider payloads ever land here.
struct AgentBridgeSettings: Decodable, Sendable {
    let masterEnabled: Bool
    let claudeEnabled: Bool
    let codexEnabled: Bool
    let grokEnabled: Bool
    let ompEnabled: Bool
    let policyGeneration: UInt64?
    let allowedProjects: [AgentBridgeProjectScope]
    let permissionRules: [AgentBridgePermissionRule]

    /// What the console shows before the first read answers.
    static let empty = AgentBridgeSettings(
        masterEnabled: false, claudeEnabled: false, codexEnabled: false, grokEnabled: false,
        ompEnabled: false, policyGeneration: nil, allowedProjects: [], permissionRules: [])

    func isEnabled(_ agent: AgentBridgeAgent) -> Bool {
        switch agent {
        case .claude: claudeEnabled
        case .codex: codexEnabled
        case .grok: grokEnabled
        case .omp: ompEnabled
        }
    }
}

/// The payload of `agent-bridge-update-event`.
struct AgentBridgeUpdate: Decodable, Sendable {
    let status: AgentBridgeStatus
}

/// The one field of `get_app_settings` this slice reads.
struct AgentBridgeAppSettings: Decodable, Sendable {
    let agentBridge: AgentBridgeSettings?
}

/// How this slice draws what the bridge hands it.
enum AgentBridgeFormat {
    /// Clock time to the second, the resolution a request's expiry is read at.
    static let expiry: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm:ss"
        return formatter
    }()

    /// An opaque id where only a name is needed, as in a menu of sessions.
    static func short(_ id: String) -> String {
        id.count <= 14 ? id : "\(id.prefix(12))…"
    }
}

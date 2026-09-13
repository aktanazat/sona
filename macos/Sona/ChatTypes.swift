import Foundation

/// The shapes the agent panel sends, mirroring `src-tauri/src/agent_panel/wire.rs`
/// and `protocol.rs` with snake_case turned into camelCase by `Core.decoder`.
/// Enum values are the wire strings verbatim.

extension CoreEvent {
    /// The relay's own state moved: paired, offline, rate limited, ready.
    static let agentPanelStatusChanged = "agent-panel://status-changed"
    /// A turn was accepted, stepped, answered, failed or stopped.
    static let agentPanelTurnChanged = "agent-panel://turn-changed"
    /// A settings proposal appeared, applied, was undone or was refused.
    static let agentPanelProposalChanged = "agent-panel://proposal-changed"
    /// Carries nothing the chat can use, so it is only a cue to re-read
    /// `get_app_settings`.
    static let chatSettingsChanged = "settings-changed"
}

/// Which capability-scoped brain a turn is addressed to. The reader chooses:
/// one answers questions from their own corpus, the other proposes settings
/// changes and can change nothing without a card and a press.
enum AgentPanelWorkspace: String, Codable, CaseIterable, Identifiable {
    case sonaChat = "sona_chat"
    case sonaConfig = "sona_config"

    var id: String { rawValue }

    /// The menu's word for it.
    var title: String {
        switch self {
        case .sonaChat: "Ask"
        case .sonaConfig: "Configure"
        }
    }

    /// Which sandbox is listening, said by the field rather than by a label
    /// for a control that is inside the menu.
    var prompt: String {
        switch self {
        case .sonaChat: "Ask anything"
        case .sonaConfig: "Change a setting"
        }
    }
}

enum AgentPanelRelayStatus: String, Decodable {
    case disabled, unpaired, ready
    case rateLimited = "rate_limited"
    case offline
    case invalidConfiguration = "invalid_configuration"
    case secretUnavailable = "secret_unavailable"
    case untrustedResponse = "untrusted_response"
    case workspaceMismatch = "workspace_mismatch"
    case remoteRejected = "remote_rejected"
    case ownershipRejected = "ownership_rejected"
}

/// What the relay is doing, as the chat needs to know it.
///
/// Eleven relay statuses collapse onto seven, because the chat acts on
/// exactly seven things: it has not asked yet, the agent is off, it is
/// unpaired, the relay is away, the relay is asking it to slow down,
/// something else went wrong, or it works.
enum ChatPhase {
    case loading, disabled, unpaired, offline, rateLimited, error, ready

    init(_ status: AgentPanelStatus?) {
        guard let status else {
            self = .loading
            return
        }
        switch status.relayStatus {
        case .disabled: self = .disabled
        case .unpaired: self = .unpaired
        case .offline: self = .offline
        case .rateLimited: self = .rateLimited
        case .ready: self = .ready
        /* Invalid pairing, a missing secret, an answer that failed
         * verification or came back for the workspace that did not ask, a
         * rejection from the far side: all of them mean the same thing to
         * someone looking at a chat window — it is not going to answer, and
         * Settings is where the pairing lives. */
        case .invalidConfiguration, .secretUnavailable, .untrustedResponse,
             .workspaceMismatch, .remoteRejected, .ownershipRejected:
            self = .error
        }
    }

    /// The sentence a phase owes the reader instead of a conversation.
    var notice: String? {
        switch self {
        case .disabled: "The Sona agent is switched off."
        case .unpaired: "The Sona agent is not paired with a relay."
        case .offline: "The Sona agent's relay could not be reached."
        case .rateLimited: "The relay rate limited Sona. Retrying…"
        case .error: "The Sona agent is unavailable."
        case .loading, .ready: nil
        }
    }

    /// A rate limit is temporary and the turn is already retrying, so it
    /// offers no second action.
    var offersRetry: Bool { self == .offline || self == .error }

    /// Configuration failures still point at the one screen that owns the
    /// switch, the pairing, the address and the pinned key.
    var offersSettings: Bool { notice != nil && self != .rateLimited }
}

enum AgentPanelTurnState: String, Decodable {
    case submitting, queued, leased, running
    case waitingUser = "waiting_user"
    case waitingApproval = "waiting_approval"
    case canceling, succeeded, failed, canceled
    case unverifiedExternal = "unverified_external"

    /// A live turn can be stopped; a finished one is a record, and offering
    /// to stop it is offering to undo the past.
    var isTerminal: Bool {
        switch self {
        case .succeeded, .failed, .canceled, .unverifiedExternal: true
        case .submitting, .queued, .leased, .running, .waitingUser, .waitingApproval, .canceling: false
        }
    }

    var label: String {
        switch self {
        case .submitting: "Sending"
        case .queued: "Queued"
        case .leased: "Picked up"
        case .running: "Thinking"
        case .waitingUser: "Waiting for you"
        case .waitingApproval: "Waiting for approval"
        case .canceling: "Stopping"
        case .succeeded: "Answered"
        case .failed: "Failed"
        case .canceled: "Stopped"
        case .unverifiedExternal: "Unverified reply"
        }
    }
}

/// Why a turn ended with nothing to read.
enum AgentPanelTurnFailure: String, Decodable {
    case unreachable, refused, failed
    case tooManyLookups = "too_many_lookups"
    case rateLimited = "rate_limited"

    var message: String {
        switch self {
        case .unreachable: "Sona couldn't connect."
        case .refused: "Sona couldn't take that question."
        case .failed: "Sona couldn't finish that answer."
        case .tooManyLookups: "Sona needed more than three lookups for this question."
        case .rateLimited: "Sona was rate limited."
        }
    }
}

enum AgentPanelStepState: String, Decodable {
    case running, done, failed
}

/// One row of the chat's "Worked for Ns" disclosure. Both offsets are what
/// the core observed, measured from the turn's start.
struct AgentPanelStep: Decodable, Identifiable {
    let id: String
    let label: String
    let state: AgentPanelStepState
    let startedAfterMs: Int64
    /// Absent while the step is still running.
    let endedAfterMs: Int64?
    /// The Sona tool this step ran, for a lookup this Mac made itself. Absent
    /// on a step the relay reported.
    let tool: String?

    /// A tool's own name in the reader's words; a relay step keeps its label.
    var title: String {
        guard let tool else { return label }
        switch tool {
        case "search": return "Searching your recordings"
        case "recent": return "Listing recent recordings"
        case "meeting": return "Reading a meeting"
        case "transcript": return "Reading the transcript"
        case "person": return "Looking up a person"
        case "loops": return "Checking open loops"
        case "upcoming": return "Checking the calendar"
        case "dictation": return "Reading a dictation"
        case "word_stats": return "Counting words"
        case "activity": return "Counting activity"
        default: return label
        }
    }

    /// A tool's name is a machine's name, so it reads as one.
    var isTool: Bool { tool != nil }
}

/// Where one offered corpus change has got to. An action that has been undone
/// is an action that is not in effect, which is what dismissed already means.
enum AgentPanelActionState: String, Decodable {
    case pending, applied, dismissed
}

/// One corpus change an answer offered: what it changes, and why.
enum AgentChatAction: Decodable {
    case resolveLoop(reason: String)
    case assignLoop(reason: String)
    case setSeriesTemplate(reason: String, template: String)
    case addVocabularyTerm(reason: String, term: String)
    case renameSpeaker(reason: String, name: String)

    private enum Key: String, CodingKey {
        case kind, reason, templateId, term, replacement, name
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        let reason = try container.decode(String.self, forKey: .reason)
        switch kind {
        case "resolve_loop":
            self = .resolveLoop(reason: reason)
        case "assign_loop":
            self = .assignLoop(reason: reason)
        case "set_series_template":
            let template = try container.decode(String.self, forKey: .templateId)
            self = .setSeriesTemplate(
                reason: reason, template: Self.templates[template] ?? template)
        case "add_vocabulary_term":
            let term = try container.decode(String.self, forKey: .term)
            self = .addVocabularyTerm(
                reason: reason,
                term: try container.decodeIfPresent(String.self, forKey: .replacement) ?? term)
        case "rename_speaker":
            self = .renameSpeaker(reason: reason, name: try container.decode(String.self, forKey: .name))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown chat action \(kind)")
        }
    }

    /// The assistant's own reason for offering it, which is the sentence that
    /// says which commitment or which meeting this is about.
    var reason: String {
        switch self {
        case let .resolveLoop(reason), let .assignLoop(reason): reason
        case let .setSeriesTemplate(reason, _), let .addVocabularyTerm(reason, _),
             let .renameSpeaker(reason, _): reason
        }
    }

    /// What the card says it will change. The kind of change, never the row
    /// id: an id is a digest, and printing one would tell the reader nothing.
    var line: String {
        switch self {
        case .resolveLoop: "Mark a commitment done"
        case .assignLoop: "Give a commitment an owner"
        case let .setSeriesTemplate(_, template): "Use the \(template) template for this series"
        case let .addVocabularyTerm(_, term): "Add \"\(term)\" to your vocabulary"
        case let .renameSpeaker(_, name): "Rename a speaker to \(name)"
        }
    }

    /// The five notes templates, named the way the meetings screens name them.
    private static let templates = [
        "general": "General meeting",
        "one_on_one": "One-to-one",
        "interview": "Interview",
        "sales_call": "Sales call",
        "standup": "Standup",
    ]
}

/// One card under an answer: what was offered, whether it has happened, and
/// the receipt it produced.
struct AgentPanelAction: Decodable, Identifiable {
    /// Position in the turn's offer, which is how a command names one.
    let actionIndex: UInt32
    let action: AgentChatAction
    let state: AgentPanelActionState
    let operationId: String?

    var id: UInt32 { actionIndex }
}

struct AgentPanelTurnStatus: Decodable {
    let turnId: String
    let workspace: AgentPanelWorkspace
    let state: AgentPanelTurnState
    let eventCursor: UInt64
    let startedAtUtcMs: Int64
    /// When it reached a terminal state. A finished turn's elapsed time is a
    /// fact about the past, fixed by the core.
    let completedAtUtcMs: Int64?
    let steps: [AgentPanelStep]
    let actions: [AgentPanelAction]
    let failure: AgentPanelTurnFailure?

    var isRunning: Bool { !state.isTerminal }

    /// How long the turn took, in milliseconds.
    func workedMs(_ now: Int64) -> Int64 {
        max(0, (completedAtUtcMs ?? now) - startedAtUtcMs)
    }

    /// How long one step took, on the same axis.
    func workedMs(_ step: AgentPanelStep, _ now: Int64) -> Int64 {
        max(0, (step.endedAfterMs ?? workedMs(now)) - step.startedAfterMs)
    }

    /// A live turn always gets a timing line; a finished one gets one when
    /// the core recorded its finish.
    var showsTiming: Bool { isRunning || completedAtUtcMs != nil }

    /// "Thinking · 5s" while it runs, "Worked for 5s" once it is over.
    func timing(_ now: Int64) -> String {
        isRunning
            ? "\(state.label) · \(Self.seconds(workedMs(now)))s"
            : "Worked for \(Self.seconds(workedMs(now)))s"
    }

    /// The wait message is a promise of the shell, never a relay timeout.
    func isStillWaiting(_ now: Int64) -> Bool {
        steps.isEmpty
            && (state == .queued || state == .running)
            && workedMs(now) >= Self.stillWaitingAfterMs
    }

    static let stillWaitingAfterMs: Int64 = 30_000

    static func seconds(_ milliseconds: Int64) -> Int {
        Int((Double(milliseconds) / 1000).rounded())
    }
}

enum AgentPanelProposalState: String, Decodable {
    case pending, applied, undone, rejected

    var label: String {
        switch self {
        case .pending: "Not applied yet"
        case .applied: "Applied"
        case .undone: "Undone"
        case .rejected: "Not applied"
        }
    }
}

/// How much a settings change has to be asked about before it happens.
enum AgentPanelConfirmation: String, Decodable {
    case automatic, review, explicit
}

/// One setting a proposal would move. The key stays verbatim: it is an
/// identifier, and naming the set is what makes one Apply honest.
struct AgentPanelSettingChange: Decodable {
    let key: String
}

struct AgentPanelProposal: Decodable {
    let proposalId: String
    let summary: String
    let rationale: String
    let actions: [AgentPanelSettingChange]
    let followUpQuestion: String?
    let sourceSettingsRevision: UInt64
    let confirmation: AgentPanelConfirmation
    let state: AgentPanelProposalState
    let receiptId: String?
    let appliedRevision: UInt64?

    /// The settings one Apply moves.
    var changeKeys: String { actions.map(\.key).joined(separator: ", ") }

    /// Undo puts back the revision the apply produced, or the one it was
    /// built against when nothing has been applied yet.
    var undoRevision: UInt64 { appliedRevision ?? sourceSettingsRevision }
}

enum AgentChatRole: String, Decodable {
    case user, assistant
}

/// The terminal result remembered with an earlier question.
enum AgentChatOutcome: Decodable {
    case failure(AgentPanelTurnFailure)
    case canceled

    private enum Key: String, CodingKey { case kind, failure }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        let kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "failure":
            self = .failure(try container.decode(AgentPanelTurnFailure.self, forKey: .failure))
        case "canceled":
            self = .canceled
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: container, debugDescription: "unknown chat outcome \(kind)")
        }
    }
}

struct AgentChatTurn: Decodable {
    let role: AgentChatRole
    let message: String
    let outcome: AgentChatOutcome?
}

/// One row of the history menu: enough to choose by, and no transcript.
struct AgentChatConversationSummary: Decodable, Identifiable {
    let conversationId: String
    let title: String
    let updatedAtUtcMs: Int64

    var id: String { conversationId }
}

struct AgentPanelStatus: Decodable {
    let invalidationId: UInt64
    let relayStatus: AgentPanelRelayStatus
    let conversationId: String?
    let conversation: [AgentChatTurn]
    /// An action's command answers with the turn alone, so the held status is
    /// patched at the one field that moved rather than re-read.
    var turn: AgentPanelTurnStatus?
    let proposal: AgentPanelProposal?
}

/// The reason a command refused, and the line the chat shows for it.
///
/// `unauthorized_window` is not here: it named a webview that asked from a
/// window it did not own, and the native shell is the only caller on this
/// socket.
enum AgentPanelCommandError: String, Decodable {
    case disabled, unpaired, offline
    case invalidConfiguration = "invalid_configuration"
    case secretUnavailable = "secret_unavailable"
    case untrustedResponse = "untrusted_response"
    case workspaceMismatch = "workspace_mismatch"
    case remoteRejected = "remote_rejected"
    case ownershipRejected = "ownership_rejected"
    case unknownConversation = "unknown_conversation"
    case invalidRequest = "invalid_request"
    case turnActive = "turn_active"
    case unknownTurn = "unknown_turn"
    case unknownProposal = "unknown_proposal"
    case unknownAction = "unknown_action"
    case actionFailed = "action_failed"
    case confirmationRequired = "confirmation_required"
    case staleProposal = "stale_proposal"
    case invalidProposal = "invalid_proposal"
    case invalidSetting = "invalid_setting"
    case notUndoable = "not_undoable"

    /// The ten pairing reasons are worded once, where the pairing screen
    /// keeps them. The rest share one sentence, as the React sheet did.
    var message: String {
        switch self {
        case .disabled: "The agent panel is off."
        case .unpaired: "No server is paired yet."
        case .offline: "The server didn't reply."
        case .invalidConfiguration: "That address or key is not one Sona will use."
        case .secretUnavailable: "This Mac's key could not be read."
        case .untrustedResponse: "The reply was not signed by the paired server."
        case .workspaceMismatch: "The reply did not match the question that was asked."
        case .remoteRejected: "The server refused the request."
        case .ownershipRejected: "The server replied for someone else."
        case .unknownConversation, .invalidRequest, .turnActive, .unknownTurn, .unknownProposal,
             .unknownAction, .actionFailed, .confirmationRequired, .staleProposal, .invalidProposal,
             .invalidSetting, .notUndoable:
            "Sona couldn't finish that answer."
        }
    }
}

struct AgentPanelPublicIdentity: Decodable {
    let keyId: String
    let publicKey: String
}

enum AgentPairingCommand: String, Decodable {
    case set = "set"
    case clear = "clear"
    case testConnection = "test_connection"
}

enum AgentPairingActor: String, Decodable {
    case user
}

/// What the panel is paired to, as the settings store holds it. The private
/// half of this Mac's identity is never here.
struct AgentPairingStatus: Decodable {
    let paired: Bool
    let relayUrl: String?
    let relayKeyId: String?
    let relayPublicKey: String?
    let lastSuccessfulConnectionAtUtcMs: Int64?
}

/// Proof that a pairing change happened: what was asked, by whom, when it
/// committed, and the state it left behind.
struct AgentPairingReceipt: Decodable {
    let schemaVersion: UInt32
    let receiptId: String
    let command: AgentPairingCommand
    let actor: AgentPairingActor
    let requestedAtUtcMs: Int64
    let committedAtUtcMs: Int64
    let pairing: AgentPairingStatus
}

/// The evidence one Ask turn carries, as `sona_query_pack` builds it.
struct ChatContextPack: Decodable {
    /// The pack itself, ready to ride as a turn's context.
    let pack: String
    /// Exactly the rows quoted in the pack. Their number is all the chat
    /// shows, so they stay as the core sent them.
    let sources: [JSONValue]
}

/// The settings this slice reads. `settings-changed` says only that something
/// moved, so these are fetched again whole.
struct ChatSettings: Decodable {
    let agentPanelEnabled: Bool?
    let agentPanelRelayUrl: String?
    let agentPanelRelayKeyId: String?
    let agentPanelRelayPublicKey: String?
    let agentPanelPaired: Bool?
    let agentPanelLastSuccessfulConnectionAt: Int64?
    let meetingRemoteIntelligenceEnabled: Bool?
    /// The language the reader chose for the app, which is the language a
    /// turn asks to be answered in.
    let appLanguage: String?

    var isEnabled: Bool { agentPanelEnabled ?? true }
    var isPaired: Bool { agentPanelPaired ?? false }
    /// Whether matching quotes may leave this Mac.
    var allowsRemoteIntelligence: Bool { meetingRemoteIntelligenceEnabled ?? false }
    var relayUrl: String { agentPanelRelayUrl ?? "" }
    var relayKeyId: String { agentPanelRelayKeyId ?? "" }
    var relayPublicKey: String { agentPanelRelayPublicKey ?? "" }
    /// The core asks in this language too when it submits a turn itself.
    var language: String { appLanguage ?? "en" }

    /// The last successful *test*, which is the only thing that stamps it.
    var lastTested: Date? {
        agentPanelLastSuccessfulConnectionAt.map {
            Date(timeIntervalSince1970: Double($0) / 1000)
        }
    }
}

/// A row of the scrollback with an identity that survives a re-read: the same
/// role saying the same words twice is one exchange, not one row drawn twice,
/// so the occurrence count is part of the identity.
struct ChatRow: Identifiable {
    let id: String
    let turn: AgentChatTurn

    static func rows(_ conversation: [AgentChatTurn]) -> [ChatRow] {
        var occurrences: [String: Int] = [:]
        return conversation.map { turn in
            let identity = "\(turn.role.rawValue)\u{1F}\(turn.message)"
            let occurrence = occurrences[identity, default: 0]
            occurrences[identity] = occurrence + 1
            return ChatRow(id: "\(identity)\u{1F}\(occurrence)", turn: turn)
        }
    }
}

/// One message as alternating prose and `sona://` addresses, in the order it
/// was written.
///
/// An answer worth reading cites where it came from, and the pack it was
/// given is nothing but quotes with `sona://` addresses beside them. Left as
/// text they are unclickable noise; split out here they are the one gesture
/// that turns an answer back into the meeting it came from.
enum ChatSegment {
    case text(String)
    case link(String)

    static func scan(_ message: String) -> [ChatSegment] {
        var segments: [ChatSegment] = []
        /// Where the prose that has not been emitted yet begins.
        var prose = message.startIndex
        var search = message.startIndex
        while let found = message.range(of: scheme, range: search..<message.endIndex) {
            var end = found.upperBound
            while end < message.endIndex, !message[end].isWhitespace, !stops.contains(message[end]) {
                end = message.index(after: end)
            }
            /* Trailing sentence punctuation is trimmed after the fact rather
             * than excluded: a `?` can legitimately open a query string,
             * while a `.` at the end of a sentence never belongs to the
             * address. */
            while end > found.upperBound, trailing.contains(message[message.index(before: end)]) {
                end = message.index(before: end)
            }
            if end == found.upperBound {
                // The bare scheme addresses nothing, so it stays in the prose.
                search = found.upperBound
                continue
            }
            if found.lowerBound > prose {
                segments.append(.text(String(message[prose..<found.lowerBound])))
            }
            segments.append(.link(String(message[found.lowerBound..<end])))
            prose = end
            search = end
        }
        if prose < message.endIndex {
            segments.append(.text(String(message[prose...])))
        }
        return segments
    }

    private static let scheme = "sona://"
    private static let stops: Set<Character> = ["<", ">", "\"", "'", "`", ")", "]"]
    private static let trailing: Set<Character> = [".", ",", ";", ":", "!", "?"]
}

import Foundation
import Observation

struct PlanningSourceRef: Codable, Equatable {
    let kind: PlanningSourceKind
    let id: String
}

struct PlanningPassage: Codable, Equatable {
    let id: String
    let atMs: Int64?
    let contentHash: String
}

struct PlanningReviewSource: Codable, Equatable {
    let source: PlanningSourceRef
    let title: String
    let revision: Int64
    let contentHash: String
    let passages: [PlanningPassage]
}

struct PlanningCitation: Codable, Equatable {
    var source: UInt32
    var passageId: String
}

struct PlanningProjectDraft: Codable, Equatable {
    var title: String
    var description: String?
    var citations: [PlanningCitation]
}

struct PlanningMilestoneDraft: Codable, Equatable {
    var key: String
    var title: String
    var dueAtMs: Int64?
    var citations: [PlanningCitation]
}

struct PlanningTaskDraft: Codable, Equatable {
    var key: String
    var milestoneKey: String?
    var title: String
    var details: String?
    var dueAtMs: Int64?
    var ownerPersonId: String?
    var ownerIsSelf: Bool
    var citations: [PlanningCitation]
}

struct PlanningLinkedCommitmentDraft: Codable, Equatable {
    let loopId: String
    let title: String
    var milestoneKey: String?
    var citations: [PlanningCitation]
}

struct PlanningPreparedDraft: Codable, Equatable {
    let goal: String
    let sources: [PlanningReviewSource]
    var project: PlanningProjectDraft
    var milestones: [PlanningMilestoneDraft]
    var tasks: [PlanningTaskDraft]
    var linkedCommitments: [PlanningLinkedCommitmentDraft]
}

private struct PlanningPrepareParams: Encodable {
    let goal: String
    let selectedSources: [PlanningSourceRef]
}

private struct PlanningReviseParams: Encodable {
    let draft: PlanningPreparedDraft
    let requests: [String]
}

struct PlanningConversationTurn: Identifiable {
    enum Content {
        case user(String)
        case plan(PlanningPreparedDraft)
        case notice(String)
    }

    let id = UUID()
    var content: Content
}

private struct PlanningAcceptParams: Encodable {
    let draft: PlanningPreparedDraft
    let operationId: String
}

@MainActor
@Observable
final class PlannerAgentStore {
    enum Phase { case idle, preparing, revising, accepting }

    private(set) var goal = ""
    private(set) var selectedSources: [PlanningSourceChoice] = []
    private(set) var draft: PlanningPreparedDraft?
    private(set) var proposedRevision: PlanningPreparedDraft?
    private(set) var turns: [PlanningConversationTurn] = []
    private(set) var currentPlanTurnId: UUID?
    private(set) var revisionTurnId: UUID?
    private(set) var phase: Phase = .idle
    private(set) var error: String?
    private(set) var voice: ChatVoiceSession?
    private(set) var stoppingVoice = false
    private(set) var savedProjectId: String?
    private(set) var active = false

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var sequence = 0
    @ObservationIgnored private var operationId = UUID().uuidString
    @ObservationIgnored private var voiceStartTask: Task<Void, Never>?
    @ObservationIgnored private var voiceStopTask: Task<Bool, Never>?
    @ObservationIgnored private var closingVoiceId: String?
    @ObservationIgnored private var preserveDraftOnClose = false
    @ObservationIgnored private var acceptedRequests: [String] = []
    @ObservationIgnored private var pendingRequest: String?

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.chatVoice) { [weak self] line in
            guard let self, self.voice != nil else { return }
            do { self.receiveVoice(try Core.payload(line)) }
            catch {
                self.error = Self.message(error)
                self.stopVoice()
            }
        }
    }

    var busy: Bool { phase != .idle }
    var hasVoiceToStop: Bool { voice != nil || closingVoiceId != nil }
    var reviewDraft: PlanningPreparedDraft? { proposedRevision ?? draft }
    var hasUnsentMessage: Bool { !goal.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    var canPrepare: Bool {
        active && !busy && savedProjectId == nil && proposedRevision == nil && hasUnsentMessage
    }
    var canApprove: Bool {
        active && !busy && !stoppingVoice && !hasVoiceToStop && savedProjectId == nil
            && proposedRevision == nil && !hasUnsentMessage && draft.map(Self.validForApproval) == true
    }
    var voiceLabel: String? {
        if stoppingVoice { return "Stopping microphone…" }
        guard let voice else { return nil }
        switch voice.phase {
        case .starting: return "Starting microphone…"
        case .transcribing: return "Transcribing on this Mac…"
        default: return "Listening…"
        }
    }

    func activate(goal seed: String? = nil) {
        active = true
        if let seed { reset(goal: seed) }
    }

    func deactivate() {
        active = false
        sequence += 1
        if phase == .preparing || phase == .revising { phase = .idle }
        if !preserveDraftOnClose { clearConversation() }
        preserveDraftOnClose = false
        error = nil
        stopVoice()
    }

    func preserveReviewForSource() {
        preserveDraftOnClose = true
        stopVoice()
    }

    func reset(goal seed: String = "") {
        guard phase != .accepting else { return }
        sequence += 1
        phase = .idle
        goal = seed
        selectedSources = []
        savedProjectId = nil
        preserveDraftOnClose = false
        clearConversation()
        operationId = UUID().uuidString
        error = nil
        stopVoice()
    }

    func editGoal(_ text: String) {
        guard !busy, goal != text else { return }
        stopVoice()
        goal = text
        error = nil
    }

    func selectSources(_ sources: [PlanningSourceChoice]) {
        guard !busy, draft == nil, sources.count <= 12, selectedSources != sources else { return }
        selectedSources = sources
        error = nil
    }

    func edit(_ change: (inout PlanningPreparedDraft) -> Void) {
        guard !busy, var next = reviewDraft else { return }
        change(&next)
        guard next != reviewDraft else { return }
        if proposedRevision != nil { proposedRevision = next }
        else { draft = next }
        operationId = UUID().uuidString
        error = nil
    }

    func removeMilestone(_ key: String) {
        edit { draft in
            draft.milestones.removeAll { $0.key == key }
            for index in draft.tasks.indices where draft.tasks[index].milestoneKey == key {
                draft.tasks[index].milestoneKey = nil
            }
            for index in draft.linkedCommitments.indices where draft.linkedCommitments[index].milestoneKey == key {
                draft.linkedCommitments[index].milestoneKey = nil
            }
        }
    }

    func addMilestone() {
        edit { draft in
            guard draft.milestones.count < 16 else { return }
            draft.milestones.append(PlanningMilestoneDraft(
                key: UUID().uuidString, title: "", dueAtMs: nil, citations: []))
        }
    }

    func addTask() {
        edit { draft in
            guard draft.tasks.count < 32 else { return }
            draft.tasks.append(PlanningTaskDraft(
                key: UUID().uuidString, milestoneKey: nil, title: "", details: nil,
                dueAtMs: nil, ownerPersonId: nil, ownerIsSelf: false, citations: []))
        }
    }

    func prepare(after stopOtherVoice: @MainActor () async -> Bool) async {
        guard canPrepare else { return }
        sequence += 1
        let request = sequence
        let text = goal.trimmingCharacters(in: .whitespacesAndNewlines)
        guard text.utf8.count <= 4_096 else {
            error = "That message is too long. Shorten it before sending."
            return
        }
        let current = draft
        phase = current == nil ? .preparing : .revising
        error = nil
        defer { if sequence == request { phase = .idle } }
        guard await stopOtherVoice(), active, sequence == request else {
            if active, sequence == request { error = "Couldn't stop the microphone. Try again." }
            return
        }
        guard await stopVoiceAndWait(), active, sequence == request else { return }
        updateCurrentTurn()
        turns.append(PlanningConversationTurn(content: .user(text)))
        do {
            let prepared: PlanningPreparedDraft
            if let current {
                prepared = try await core.request("planning_revise",
                    PlanningReviseParams(draft: current, requests: acceptedRequests + [text]))
            } else {
                prepared = try await core.request("planning_prepare",
                    PlanningPrepareParams(goal: text, selectedSources: selectedSources.map {
                        PlanningSourceRef(kind: $0.kind, id: $0.sourceId)
                    }))
            }
            guard active, sequence == request else { return }
            let turn = PlanningConversationTurn(content: .plan(prepared))
            turns.append(turn)
            goal = ""
            if current != nil {
                proposedRevision = prepared
                pendingRequest = text
                revisionTurnId = turn.id
            } else {
                draft = prepared
                currentPlanTurnId = turn.id
                operationId = UUID().uuidString
            }
        } catch {
            guard active, sequence == request else { return }
            self.error = Self.message(error)
            if current != nil {
                turns.append(PlanningConversationTurn(content: .notice("The revision failed. Your previous draft is unchanged.")))
            }
        }
    }

    func useRevision() {
        guard !busy, let proposedRevision else { return }
        draft = proposedRevision
        self.proposedRevision = nil
        currentPlanTurnId = revisionTurnId
        revisionTurnId = nil
        if let pendingRequest { acceptedRequests.append(pendingRequest) }
        pendingRequest = nil
        operationId = UUID().uuidString
        error = nil
        updateCurrentTurn()
    }

    func keepPreviousDraft() {
        guard !busy, proposedRevision != nil, let draft else { return }
        if let index = turns.firstIndex(where: { $0.id == revisionTurnId }) {
            turns[index].content = .notice("Revision declined.")
        }
        proposedRevision = nil
        revisionTurnId = nil
        pendingRequest = nil
        let turn = PlanningConversationTurn(content: .plan(draft))
        turns.append(turn)
        currentPlanTurnId = turn.id
        error = nil
    }

    private func updateCurrentTurn() {
        guard let draft, let index = turns.firstIndex(where: { $0.id == currentPlanTurnId }) else { return }
        turns[index].content = .plan(draft)
    }

    func approve() async -> String? {
        guard canApprove, let draft else { return nil }
        let request = sequence
        phase = .accepting
        error = nil
        defer { phase = .idle }
        do {
            let receipt: PlanningProjectDetail = try await core.request(
                "planning_accept", PlanningAcceptParams(draft: draft, operationId: operationId))
            savedProjectId = receipt.project.id
            guard active, sequence == request else { return nil }
            return receipt.project.id
        } catch {
            guard active, sequence == request else { return nil }
            self.error = Self.message(error)
            return nil
        }
    }

    func startVoice() {
        guard active, !busy, !stoppingVoice, !hasVoiceToStop, savedProjectId == nil, proposedRevision == nil else { return }
        let id = UUID().uuidString
        voice = ChatVoiceSession(id: id, conversationId: nil)
        error = nil
        voiceStartTask = Task {
            do { try await core.request("chat_voice_start", ["sessionId": id]) }
            catch {
                guard voice?.id == id else { return }
                voice = nil
                self.error = Self.message(error)
            }
        }
    }

    func stopVoice() {
        guard !stoppingVoice, let id = voice?.id ?? closingVoiceId else { return }
        voice = nil
        closingVoiceId = id
        stoppingVoice = true
        let start = voiceStartTask
        voiceStopTask = Task {
            await start?.value
            defer { stoppingVoice = false }
            do {
                try await core.request("chat_voice_stop", ["sessionId": id])
                closingVoiceId = nil
                return true
            } catch {
                self.error = Self.message(error)
                return false
            }
        }
    }

    func stopVoiceAndWait() async -> Bool {
        stopVoice()
        return await voiceStopTask?.value ?? true
    }

    private func receiveVoice(_ event: ChatVoiceEvent) {
        guard active, voice?.receive(event) == true else { return }
        switch event.phase {
        case .transcript:
            guard let text = event.text?.trimmingCharacters(in: .whitespacesAndNewlines), !text.isEmpty else { return }
            goal = goal.isEmpty ? text : goal + "\n" + text
            stopVoice()
        case .failed, .stopped:
            stopVoice()
            if let failure = event.error { error = failure }
        default:
            break
        }
    }

    private func clearConversation() {
        draft = nil
        proposedRevision = nil
        turns = []
        currentPlanTurnId = nil
        revisionTurnId = nil
        acceptedRequests = []
        pendingRequest = nil
    }


    private static func validForApproval(_ draft: PlanningPreparedDraft) -> Bool {
        let titles = [draft.project.title] + draft.milestones.map(\.title) + draft.tasks.map(\.title)
        return titles.allSatisfy {
            !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && $0.utf8.count <= 256
        }
    }

    private static func message(_ error: Error) -> String {
        (error as? CoreError)?.remote(as: String.self) ?? error.localizedDescription
    }
}

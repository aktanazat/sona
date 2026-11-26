import Foundation

/// What the Today screen needs from the vault: send a decision to the Mac that listed
/// the row, and delete one the Mac has answered.
@MainActor
protocol TodayTransport: AnyObject {
    /// Queues the decision and returns its object id.
    func send(_ action: WorkActionManifest) async throws -> String
    /// Throws until the object is in this iPhone's copy of the vault and its delete went through.
    func deleteObject(_ objectId: String) async throws
}

/// The phone's side of the Mac's Today: decisions on their way, and what the Mac said
/// about each. A row with a decision on its way shows that instead of its buttons, until
/// the Mac's snapshot names the decision as applied or refused.
@MainActor
final class PhoneToday: ObservableObject {
    /// How long the Mac normally needs to see a decision: it checks the vault every few
    /// seconds while it is awake.
    static let pickUpWindow: TimeInterval = 120

    struct Pending: Codable, Equatable {
        /// The task, session or inbox row the decision is about.
        let row: String
        let objectId: String
        let action: WorkActionManifest.Action
        let title: String
        let sentAt: Date
    }

    @Published private(set) var pending: [Pending] = []
    /// The decisions the Mac would not take, by title, until the user dismisses them.
    @Published var refused: [String] = []

    private let transport: TodayTransport
    private let file: URL
    private let now: () -> Date
    /// Answered decisions whose objects are still to be deleted from the vault.
    private var answered: [String] = []

    private struct Stored: Codable {
        var pending: [Pending]
        var answered: [String]
    }

    init(transport: TodayTransport, file: URL, now: @escaping () -> Date = Date.init) {
        self.transport = transport
        self.file = file
        self.now = now
        if let bytes = try? Data(contentsOf: file), let stored = try? JSONDecoder().decode(Stored.self, from: bytes) {
            pending = stored.pending
            answered = stored.answered
        }
    }

    func pending(for row: String) -> Pending? { pending.first { $0.row == row } }

    /// Whether the Mac has had its usual time to see this decision and has not.
    func isLate(_ decision: Pending) -> Bool {
        now().timeIntervalSince(decision.sentAt) > Self.pickUpWindow
    }

    /// Sends one decision about `row` to the Mac that wrote `today`. A row that already
    /// has a decision on its way takes no second one.
    func decide(
        _ action: WorkActionManifest.Action, row: String, title: String, in today: WorkToday, deviceId: String,
        taskId: String? = nil, expectedRevision: Int64? = nil, noticeIds: [String] = [],
        sessionId: String? = nil, body: String? = nil, value: String? = nil
    ) async throws {
        guard pending(for: row) == nil else { return }
        let sentAt = now()
        let manifest = WorkActionManifest(
            acted_at_utc_ms: Int64(sentAt.timeIntervalSince1970 * 1000), action: action, body: body,
            device_id: deviceId, expected_revision: expectedRevision, format_version: 1,
            kind: WorkActionObject.kind, notice_ids: noticeIds, session_id: sessionId,
            target_device_id: today.source_device_id, task_id: taskId, value: value
        )
        let objectId = try await transport.send(manifest)
        pending.append(Pending(row: row, objectId: objectId, action: action, title: title, sentAt: sentAt))
        save()
    }

    /// Reads what the Mac's newest snapshot says about each decision on its way, then
    /// deletes every answered decision the vault lets this iPhone delete yet.
    func reconcile(_ today: WorkToday?) async {
        if let today {
            let outcomes = Dictionary(today.handled.map { ($0.action_id, $0.outcome) }, uniquingKeysWith: { $1 })
            var still: [Pending] = []
            for decision in pending {
                switch outcomes[decision.objectId] {
                case .applied?:
                    answered.append(decision.objectId)
                case .refused?:
                    answered.append(decision.objectId)
                    refused.append(decision.title)
                case nil:
                    still.append(decision)
                }
            }
            if still != pending { pending = still }
        }
        var left: [String] = []
        for objectId in answered {
            do { try await transport.deleteObject(objectId) } catch { left.append(objectId) }
        }
        answered = left
        save()
    }

    func clear() {
        pending = []
        answered = []
        refused = []
        try? FileManager.default.removeItem(at: file)
    }

    private func save() {
        guard let bytes = try? JSONEncoder().encode(Stored(pending: pending, answered: answered)) else { return }
        try? bytes.write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }
}

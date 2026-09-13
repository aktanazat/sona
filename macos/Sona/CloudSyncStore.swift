import AppKit
import Foundation
import Observation
import UniformTypeIdentifiers

/// Cloud sync: what the vault is doing, the one-time tasks that create it,
/// and what each meeting is doing with it.
///
/// One pending slot, as the old panel had: any command refuses while another
/// runs, so an approval can never land on top of a bootstrap. The core's own
/// `cloud-sync:changed` is the only invalidation; every action that changes
/// the vault re-reads afterwards rather than guessing the new state.
@MainActor
@Observable
final class CloudSyncStore {
    /// What a press is waiting on, and which meeting it is for.
    enum Action: Equatable {
        case bootstrap
        case recover
        case offer
        case approveOffer
        case approveCandidate
        case accept
        case pause
        case retry(String)
        case conflict(String)
        case exportBundle(String)
        case importBundle
        case browserShare(String)
        case revoke
    }

    private(set) var overview: CloudSyncOverview?
    /// Provisioning on this device: read-only, never a switch.
    private(set) var service: CloudSyncServiceStatus?
    /// Every meeting the vault knows about, in the order the core lists them.
    private(set) var statuses: [CloudSyncMeetingStatus] = []
    /// Why there is no meeting list, when the core would not give one.
    private(set) var statusNote: String?
    /// True until the first overview lands, so the status word can say so.
    private(set) var loading = true
    private(set) var pending: Action?

    /// Shown once and never again: the core does not store it.
    private(set) var recoveryCode: String?
    /// The record this Mac minted for another device to read.
    private(set) var offer: PairingOffer?
    /// The record another device showed, and this Mac's own reading of it.
    private(set) var candidate: PairingCandidate = .empty
    private(set) var candidateOffer = ""
    private(set) var bundle: CloudShareFile?
    private(set) var browserShare: CloudShareLink?
    private(set) var importedSessionId: String?

    var endpoint = ""
    var bootstrapSecret = ""
    var recoveryInput = ""
    var vaultId = ""
    var receivedOffer = ""
    /// A week out, the default the old panel opened with.
    var shareExpiry = Date().addingTimeInterval(7 * 24 * 60 * 60)

    /// The last thing the core could not do, shown until the next success. A
    /// command's refusal outlives a reload, the way the old panel kept its
    /// command error beside a resource that had since refreshed.
    var error: String? { commandError ?? resourceError }
    private(set) var commandError: String?
    private(set) var resourceError: String?
    private(set) var titles: [String: String] = [:]

    @ObservationIgnored private let core: Core
    /// Drops the answer to a paste a newer paste replaced: a stale
    /// fingerprint beside a newer code is the one thing this must not show.
    @ObservationIgnored private var candidateRead = 0
    /// Drops the answer to a reload a newer reload replaced.
    @ObservationIgnored private var refreshTicket = 0

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.cloudSyncChanged) { [weak self] line in self?.apply(line) }
    }

    func start() async {
        await refresh()
    }

    /// The one line a reader checks on the way past.
    var accountStatus: CloudSyncAccountStatus {
        if loading { return .loading }
        if overview?.terminalError != nil { return .attention }
        if let overview, overview.enabled { return overview.paused ? .paused : .ready }
        return error == nil ? .local : .unavailable
    }

    /// Every action is refused while one runs, so every button says so.
    var busy: Bool { pending != nil }

    /// The name of a meeting, or its session id while the name is unknown.
    func title(for sessionId: String) -> String {
        titles[sessionId] ?? sessionId
    }

    // MARK: reading

    /// Everything the panel shows, in one pass.
    func refresh() async {
        refreshTicket += 1
        let ticket = refreshTicket
        await loadOverview(ticket)
        await loadService(ticket)
        await loadMeetings(ticket)
    }

    private func loadOverview(_ ticket: Int) async {
        do {
            let result: CloudSyncOverview = try await core.request("cloud_sync_overview_get")
            guard ticket == refreshTicket else { return }
            overview = result
            resourceError = nil
            loading = false
        } catch let failure {
            guard ticket == refreshTicket else { return }
            overview = nil
            loading = false
            resourceError = reason(failure)
        }
    }

    private func loadService(_ ticket: Int) async {
        do {
            let result: CloudSyncServiceStatus = try await core.request("cloud_sync_service_status")
            guard ticket == refreshTicket else { return }
            service = result
        } catch let failure {
            guard ticket == refreshTicket else { return }
            service = nil
            resourceError = reason(failure)
        }
    }

    /// The meetings and their sync state. A refusal here is a state, not a
    /// failed instruction — a Mac that never set sync up has no vault to
    /// list — so the section says why instead of the page turning red.
    private func loadMeetings(_ ticket: Int) async {
        do {
            let result: [CloudSyncMeetingStatus] = try await core.request("cloud_sync_meeting_status_list")
            guard ticket == refreshTicket else { return }
            statuses = result
            statusNote = nil
        } catch let failure {
            guard ticket == refreshTicket else { return }
            statuses = []
            statusNote = reason(failure)
        }
        await loadTitles(ticket)
    }

    /// Names for the rows. `meeting_list` walks the same page the status list
    /// does, so a row with no name here is a meeting that went away between
    /// the two calls; that row states its session id instead.
    private func loadTitles(_ ticket: Int) async {
        guard !statuses.isEmpty else { return }
        guard let page: CloudSyncMeetingPage = try? await core.request("meeting_list", ["limit": 100]) else {
            return
        }
        guard ticket == refreshTicket else { return }
        titles = Dictionary(
            page.entries.map { ($0.sessionId, $0.title) },
            uniquingKeysWith: { first, _ in first })
    }

    /// One meeting, read again: the row that just opened.
    func refreshStatus(_ sessionId: String) async {
        do {
            let status: CloudSyncMeetingStatus = try await core.request(
                "cloud_sync_meeting_status_get", ["sessionId": sessionId])
            patch(status)
        } catch let failure {
            resourceError = reason(failure)
        }
    }

    // MARK: setting up

    func bootstrap() async {
        let address = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty, !bootstrapSecret.isEmpty else { return }
        let secret = bootstrapSecret
        await call(.bootstrap) {
            let result: CloudSyncBootstrapResult = try await core.request(
                "cloud_sync_bootstrap",
                CloudSyncRequest(request: CloudSyncBootstrapBody(endpoint: address, bootstrapSecret: secret)))
            bootstrapSecret = ""
            recoveryCode = result.recoveryCode
            overview = result.overview
            await refresh()
        }
    }

    func recover() async {
        let address = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty, !recoveryInput.isEmpty else { return }
        let code = recoveryInput
        await call(.recover) {
            let result: CloudSyncOverview = try await core.request(
                "cloud_sync_recover",
                CloudSyncRequest(request: CloudSyncRecoveryBody(endpoint: address, recoveryCode: code)))
            recoveryInput = ""
            overview = result
            await refresh()
        }
    }

    func togglePaused() async {
        guard let current = overview, current.enabled else { return }
        await call(.pause) {
            let result: CloudSyncOverview = try await core.request(
                current.paused ? "cloud_sync_resume" : "cloud_sync_pause")
            overview = result
            await refresh()
        }
    }

    // MARK: pairing

    /// This Mac's own offer, for a device that will read it off this screen.
    func createOffer() async {
        let address = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        let vault = vaultId.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty, !vault.isEmpty else { return }
        await call(.offer) {
            let result: PairingOffer = try await core.request(
                "cloud_sync_pairing_offer",
                CloudSyncRequest(request: PairingOfferBody(endpoint: address, vaultId: vault)))
            offer = result
        }
    }

    /// Hands the vault root to the device the offer on screen names.
    func approveOffer() async {
        guard let current = offer else { return }
        await call(.approveOffer) {
            let result: CloudSyncOverview = try await core.request(
                "cloud_sync_pairing_approve",
                CloudSyncRequest(request: PairingApproveBody(offer: current)))
            overview = result
            await refresh()
        }
    }

    /// Every paste is read straight away, so the fingerprint is on screen
    /// before the reader decides. The fingerprint comes from the core's
    /// reading of the record, never from the `fingerprint` the paste carried.
    func readCandidate(_ text: String) {
        candidateOffer = text
        candidateRead += 1
        let read = candidateRead
        if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            candidate = .empty
            return
        }
        guard let parsed = PairingOffer.parse(text) else {
            candidate = .invalid
            return
        }
        candidate = .reading
        Task { [weak self] in
            guard let self else { return }
            do {
                let fingerprint: String = try await core.request(
                    "cloud_sync_pairing_fingerprint", ["offer": parsed])
                guard read == candidateRead else { return }
                candidate = .ready(fingerprint: fingerprint, offer: parsed)
            } catch {
                guard read == candidateRead else { return }
                candidate = .invalid
            }
        }
    }

    /// Approves the device whose code was pasted, on the record this Mac
    /// verified rather than on the text still in the field.
    func approveCandidate() async {
        guard let verified = candidate.offer else { return }
        await call(.approveCandidate) {
            let result: CloudSyncOverview = try await core.request(
                "cloud_sync_pairing_approve",
                CloudSyncRequest(request: PairingApproveBody(offer: verified)))
            candidateRead += 1
            candidateOffer = ""
            candidate = .approved
            overview = result
            await refresh()
        }
    }

    /// Takes an offer another device minted, so this Mac joins that vault.
    func acceptOffer() async {
        let address = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty, !receivedOffer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return
        }
        guard let parsed = PairingOffer.parse(receivedOffer) else {
            commandError = CloudSyncStore.invalidOffer
            return
        }
        await call(.accept) {
            let result: CloudSyncOverview = try await core.request(
                "cloud_sync_pairing_accept",
                CloudSyncRequest(request: PairingAcceptBody(endpoint: address, offer: parsed)))
            receivedOffer = ""
            overview = result
            await refresh()
        }
    }

    // MARK: one meeting

    func retry(_ sessionId: String) async {
        await call(.retry(sessionId)) {
            let status: CloudSyncMeetingStatus = try await core.request(
                "cloud_sync_retry", ["sessionId": sessionId])
            patch(status)
            await refresh()
        }
    }

    func resolveConflict(_ sessionId: String, choice: CloudSyncConflictChoice) async {
        await call(.conflict(sessionId)) {
            let status: CloudSyncMeetingStatus = try await core.request(
                "cloud_sync_conflict_resolve",
                CloudSyncRequest(request: CloudSyncConflictBody(sessionId: sessionId, choice: choice)))
            patch(status)
            await refresh()
        }
    }

    // MARK: shares

    /// The core writes the file, so the reader picks where before anything
    /// runs. `NSSavePanel` stands in for the webview's save dialog.
    func exportBundle(_ sessionId: String) async {
        guard pending == nil, let expiresAtUtcMs = expiry() else { return }
        guard let destination = CloudSyncStore.savePath() else { return }
        await call(.exportBundle(sessionId)) {
            let result: CloudShareResult = try await core.request(
                "cloud_share_create",
                CloudSyncRequest(request: CloudShareCreateBody(
                    sessionId: sessionId,
                    expiresAtUtcMs: expiresAtUtcMs,
                    destinationPath: destination)))
            bundle = CloudShareFile(sessionId: sessionId, result: result)
            await refresh()
        }
    }

    /// Reads a `.sona` file back in. It makes a meeting of its own, so it
    /// belongs to no row: the section owns it.
    func importBundle() async {
        guard pending == nil, let path = CloudSyncStore.openPath() else { return }
        await call(.importBundle) {
            let result: CloudShareImportResult = try await core.request(
                "cloud_share_import_file",
                CloudSyncRequest(request: CloudShareImportBody(path: path)))
            importedSessionId = result.sessionId
            await refresh()
        }
    }

    func createBrowserShare(_ sessionId: String) async {
        guard pending == nil, let expiresAtUtcMs = expiry() else { return }
        await call(.browserShare(sessionId)) {
            let result: CloudShareBrowserResult = try await core.request(
                "cloud_browser_share_create",
                CloudSyncRequest(request: CloudShareBrowserBody(
                    sessionId: sessionId, expiresAtUtcMs: expiresAtUtcMs)))
            browserShare = CloudShareLink(sessionId: sessionId, result: result)
            await refresh()
        }
    }

    /// Kills a link other people already hold. The view confirms first.
    func revokeBrowserShare() async {
        guard let link = browserShare else { return }
        await call(.revoke) {
            let result: CloudSyncOverview = try await core.request(
                "cloud_share_revoke",
                CloudSyncRequest(request: CloudShareRevokeBody(shareId: link.result.shareId)))
            browserShare = nil
            overview = result
            await refresh()
        }
    }

    // MARK: plumbing

    private static let invalidOffer = "The pairing offer is incomplete or invalid."
    private static let pastExpiry = "Choose a future expiry."

    /// The one extension the core reads and writes. A type the system has
    /// never seen resolves to nothing, and then the panel takes any file
    /// rather than none.
    private static let shareFileTypes: [UTType] = [UTType(filenameExtension: "sona")].compactMap { $0 }

    private static func savePath() -> String? {
        let panel = NSSavePanel()
        panel.title = "Export a Sona share"
        panel.nameFieldStringValue = "meeting.sona"
        panel.canCreateDirectories = true
        if !shareFileTypes.isEmpty {
            panel.allowedContentTypes = shareFileTypes
        }
        return panel.runModal() == .OK ? panel.url?.path : nil
    }

    private static func openPath() -> String? {
        let panel = NSOpenPanel()
        panel.title = "Import a Sona share"
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.canChooseFiles = true
        if !shareFileTypes.isEmpty {
            panel.allowedContentTypes = shareFileTypes
        }
        return panel.runModal() == .OK ? panel.url?.path : nil
    }

    /// The expiry as the core takes it, or nothing and a complaint. A share
    /// that has already expired is not a share.
    private func expiry() -> Int64? {
        guard shareExpiry > .now else {
            commandError = CloudSyncStore.pastExpiry
            return nil
        }
        return CloudSyncClock.utcMs(shareExpiry)
    }

    /// Runs one command in the single pending slot: the last complaint goes
    /// as it starts, the core's own reason stays if it refuses.
    private func call(_ action: Action, _ work: () async throws -> Void) async {
        guard pending == nil else { return }
        pending = action
        commandError = nil
        do {
            try await work()
        } catch let failure {
            commandError = reason(failure)
        }
        pending = nil
    }

    /// The core's own reason for refusing: the command's typed error where it
    /// has one, the bridge's message otherwise.
    private func reason(_ failure: Error) -> String {
        if let failure = failure as? CoreError, let kind = failure.remote(as: CloudSyncErrorKind.self) {
            return kind.guidance
        }
        return failure.localizedDescription
    }

    private func patch(_ status: CloudSyncMeetingStatus) {
        if let index = statuses.firstIndex(where: { $0.sessionId == status.sessionId }) {
            statuses[index] = status
        } else {
            statuses.append(status)
        }
    }

    /// `cloud-sync:changed`. One meeting's change lands on that row at once,
    /// so the state word never lags the core; the reload behind it settles
    /// the counts the payload does not carry.
    private func apply(_ line: Data) {
        if let changed: CloudSyncChanged = try? Core.payload(line),
           let sessionId = changed.sessionId,
           let state = changed.state,
           let index = statuses.firstIndex(where: { $0.sessionId == sessionId }) {
            let old = statuses[index]
            statuses[index] = CloudSyncMeetingStatus(
                sessionId: old.sessionId,
                state: state,
                remoteRevisionId: old.remoteRevisionId,
                retryAtUtcMs: old.retryAtUtcMs,
                shareCount: old.shareCount)
        }
        Task { await refresh() }
    }
}

/// Every cloud command that carries a serde struct takes it under `request`.
struct CloudSyncRequest<Body: Encodable>: Encodable {
    let request: Body
}

/// The request bodies, with the core's own field names: the request encoder
/// converts nothing, so each one spells its keys out.
struct CloudSyncBootstrapBody: Encodable {
    let endpoint: String
    let bootstrapSecret: String

    enum CodingKeys: String, CodingKey {
        case endpoint
        case bootstrapSecret = "bootstrap_secret"
    }
}

struct CloudSyncRecoveryBody: Encodable {
    let endpoint: String
    let recoveryCode: String

    enum CodingKeys: String, CodingKey {
        case endpoint
        case recoveryCode = "recovery_code"
    }
}

struct CloudSyncConflictBody: Encodable {
    let sessionId: String
    let choice: CloudSyncConflictChoice

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case choice
    }
}

struct PairingOfferBody: Encodable {
    let endpoint: String
    let vaultId: String

    enum CodingKeys: String, CodingKey {
        case endpoint
        case vaultId = "vault_id"
    }
}

struct PairingApproveBody: Encodable {
    let offer: PairingOffer
}

struct PairingAcceptBody: Encodable {
    let endpoint: String
    let offer: PairingOffer
}

struct CloudShareCreateBody: Encodable {
    let sessionId: String
    let expiresAtUtcMs: Int64
    let destinationPath: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case expiresAtUtcMs = "expires_at_utc_ms"
        case destinationPath = "destination_path"
    }
}

struct CloudShareBrowserBody: Encodable {
    let sessionId: String
    let expiresAtUtcMs: Int64

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case expiresAtUtcMs = "expires_at_utc_ms"
    }
}

struct CloudShareRevokeBody: Encodable {
    let shareId: String

    enum CodingKeys: String, CodingKey {
        case shareId = "share_id"
    }
}

struct CloudShareImportBody: Encodable {
    let path: String
}

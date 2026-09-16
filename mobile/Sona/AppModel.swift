import Combine
import Foundation
import Network
import SwiftUI
import UIKit
import WatchConnectivity

/// What the one status line has to be able to say about the outbox.
enum OutboxState: Equatable {
    case empty
    case uploading
    /// Something is on this phone that the vault has not taken yet.
    case waiting
    case saved
}

/// One owner of everything that outlives a screen: the vault state, the outbox, the
/// microphone, connectivity and the watch link.
@MainActor
final class AppModel: NSObject, ObservableObject {
    @Published private(set) var vault: VaultState
    @Published private(set) var queued: [QueuedRecording] = []
    @Published private(set) var outbox: OutboxState = .empty
    @Published private(set) var pairingOffer: PairingOffer?
    @Published var pairingMessage: LocalizedStringKey?
    @Published var endpointDraft: String
    @Published var vaultIdDraft: String
    @Published var consentAccepted: Bool
    @Published private(set) var startingRecording = false
    /// The draft the keyboard can insert, or nothing waiting for it.
    @Published private(set) var keyboardDraft: UUID?

    let recorder = PhoneRecorder()
    let dictation = PhoneDictation()
    let callOffers = CallOfferService()

    /// Set when this recording was started from a call notification.
    private var afterCall = false
    private let queue: UploadQueue
    private let monitor = NWPathMonitor()
    private static let consentKey = "sona.consent.version"
    private static let consentVersion = 1

    override init() {
        let state = VaultKeychain.loadOrMint()
        vault = state
        pairingOffer = state.pending?.offer
        endpointDraft = state.credentials?.endpoint ?? state.pending?.endpoint ?? ""
        vaultIdDraft = state.credentials?.vaultId ?? state.pending?.vaultId ?? ""
        consentAccepted =
            UserDefaults.standard.integer(forKey: AppModel.consentKey) >= AppModel.consentVersion
        queue = UploadQueue(
            root: AppModel.outboxRoot(),
            identity: state.identity,
            credentials: state.credentials
        )
        super.init()
        recorder.onInterrupted = { [weak self] in
            self?.stopRecording()
        }
        /* Tapping either call notification is the operator asking to record now, and
         * the note is titled after the call it followed. */
        callOffers.onOfferAccepted = { [weak self] in
            self?.afterCall = true
            self?.startRecording()
        }
        startConnectivityWatch()
        activateWatchSession()
        /* A launch is the operator asking again: anything the vault refused earlier gets
         * one more attempt before it is reported as still here. */
        Task {
            await queue.retryParked()
            await drain()
        }
    }

    var isPaired: Bool { vault.credentials != nil }

    var deviceId: String { vault.identity.deviceId }

    func acceptConsent() {
        UserDefaults.standard.set(AppModel.consentVersion, forKey: AppModel.consentKey)
        consentAccepted = true
        /* The one notification prompt in the app, asked where its purpose is stated. */
        Task { await callOffers.requestNotifications() }
    }

    var offersAfterCalls: Bool {
        get { callOffers.isEnabled }
        set {
            callOffers.isEnabled = newValue
            objectWillChange.send()
        }
    }

    // MARK: - Recording

    /// False while the microphone belongs to a recording, so the dictation screen can say
    /// so before the operator taps.
    var canStartDictation: Bool { !recorder.isRecording && !startingRecording }

    func startDictation() {
        guard canStartDictation else { return }
        dictation.start()
    }

    func startRecording() {
        guard !startingRecording, !recorder.isRecording else { return }
        /* A recording takes the microphone from dictation instead of refusing: the
         * transcript so far stays on the dictation screen, and an offer the operator
         * accepted from a call notification must not fail with a microphone error. */
        dictation.cancel()
        startingRecording = true
        Task {
            defer { startingRecording = false }
            let granted =
                PhoneRecorder.hasPermission
                ? true
                : await PhoneRecorder.requestPermission()
            guard granted else {
                recorder.notice = .microphoneOff
                return
            }
            do {
                try recorder.start()
                AppModel.tap()
            } catch {
                recorder.notice = .microphoneUnavailable
            }
        }
    }

    // MARK: - Keyboard draft

    /* The approved draft outlives the dictation screen, so the identity of what the
     * keyboard can insert is held here rather than in that view's state. */
    func approveForKeyboard(_ text: String) throws {
        keyboardDraft = try KeyboardDraftStore.shared().save(text: text).id
    }

    /// Withdraw what the keyboard has not taken yet. Already inserted is the same outcome.
    func withdrawKeyboardDraft() {
        guard let id = keyboardDraft else { return }
        keyboardDraft = nil
        try? KeyboardDraftStore.shared().discard(id: id)
    }

    func stopRecording() {
        guard recorder.isRecording else { return }
        guard let finished = recorder.stop() else {
            /* Stopping with nothing to keep is the one outcome that must never pass in
             * silence: the operator watched a clock run and has to be told it is gone. */
            if recorder.notice == nil { recorder.notice = .notSaved }
            return
        }
        afterCall = false
        AppModel.tap()
        Task {
            await enqueue(
                audio: finished.audio,
                recordedAtUtcMs: finished.recordedAtUtcMs,
                title: AppModel.title(
                    prefix: afterCall
                        ? NSLocalizedString("title.afterCall", comment: "")
                        : NSLocalizedString("title.phone", comment: ""),
                    utcMs: finished.recordedAtUtcMs
                )
            )
        }
    }

    private func enqueue(audio: CapturedAudio, recordedAtUtcMs: Int64, title: String) async {
        _ = try? await queue.enqueue(
            audio: audio, recordedAtUtcMs: recordedAtUtcMs, title: title
        )
        await drain()
    }

    /// Run the outbox and report where it ended up, so the one status line can say
    /// "uploading", "waiting for a connection" or "saved" without guessing.
    private func drain() async {
        queued = await queue.items()
        guard !queued.isEmpty else {
            outbox = .empty
            return
        }
        outbox = .uploading
        await queue.drain()
        queued = await queue.items()
        outbox = queued.isEmpty ? .saved : .waiting
    }

    // MARK: - Queue

    func refresh() async {
        queued = await queue.items()
        if queued.isEmpty, outbox != .saved { outbox = .empty }
        if !queued.isEmpty, outbox != .uploading { outbox = .waiting }
    }

    func drainNow() {
        Task { await drain() }
    }

    // MARK: - Pairing

    /// Mint the candidate record the desktop approves.
    func createPairingCode() async {
        let endpoint = endpointDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        let vaultId = vaultIdDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !endpoint.isEmpty, !vaultId.isEmpty else {
            pairingMessage = "pair.needFields"
            return
        }
        pairingMessage = nil
        do {
            let client = try CompanionClient(endpoint: endpoint)
            /* The Mac rejects an offer whose expiry is outside its own fifteen-minute
             * window, so the phone's clock is corrected before the record is signed. */
            try? await client.syncClock()
            let offer = try Pairing.candidateOffer(
                identity: vault.identity,
                vaultId: vaultId,
                nowUtcMs: await client.nowUtcMs()
            )
            var state = vault
            state.pending = PendingPairing(endpoint: endpoint, vaultId: vaultId, offer: offer)
            VaultKeychain.save(state)
            vault = state
            pairingOffer = offer
        } catch {
            pairingMessage = "pair.badEndpoint"
        }
    }

    /// Read the approval the desktop wrote and store the vault root it carries.
    func finishPairing() async {
        guard let pending = vault.pending else {
            pairingMessage = "pair.needFields"
            return
        }
        pairingMessage = nil
        do {
            let client = try CompanionClient(endpoint: pending.endpoint)
            let device = try await client.selfDevice(
                identity: vault.identity, vaultId: pending.vaultId
            )
            let credentials = try Pairing.acceptApproval(
                identity: vault.identity, pending: pending, selfDevice: device
            )
            var state = vault
            state.credentials = credentials
            state.pending = nil
            VaultKeychain.save(state)
            vault = state
            pairingOffer = nil
            await queue.setCredentials(credentials)
            pairingMessage = "pair.done"
            await drain()
        } catch PairingError.notApprovedYet {
            pairingMessage = "pair.notApproved"
        } catch {
            pairingMessage = "pair.failed"
        }
    }

    // MARK: - Connectivity and watch

    private func startConnectivityWatch() {
        monitor.pathUpdateHandler = { [weak self] path in
            guard path.status == .satisfied else { return }
            Task { @MainActor in self?.drainNow() }
        }
        monitor.start(queue: DispatchQueue(label: "sona.connectivity"))
    }

    private func activateWatchSession() {
        guard WCSession.isSupported() else { return }
        WCSession.default.delegate = self
        WCSession.default.activate()
    }

    private static func outboxRoot() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? FileManager.default.temporaryDirectory
        return base.appending(path: "outbox")
    }

    /// A short tap on both edges of a recording, so the phone can stay in a pocket.
    private static func tap() {
        let generator = UIImpactFeedbackGenerator(style: .medium)
        generator.prepare()
        generator.impactOccurred()
    }

    private static func title(prefix: String, utcMs: Int64) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        let stamp = formatter.string(from: Date(timeIntervalSince1970: Double(utcMs) / 1000))
        return "\(prefix) \(stamp)"
    }
}

extension AppModel: WCSessionDelegate {
    nonisolated func session(
        _ session: WCSession,
        activationDidCompleteWith activationState: WCSessionActivationState,
        error: Error?
    ) {}

    nonisolated func sessionDidBecomeInactive(_ session: WCSession) {}

    nonisolated func sessionDidDeactivate(_ session: WCSession) {
        WCSession.default.activate()
    }

    /// A watch recording joins the same outbox as a phone recording, after the phone
    /// resamples it: the watch never speaks the vault protocol.
    nonisolated func session(_ session: WCSession, didReceive file: WCSessionFile) {
        let source = FileManager.default.temporaryDirectory
            .appending(path: "watch-\(UUID().uuidString).wav")
        try? FileManager.default.copyItem(at: file.fileURL, to: source)
        let recordedAtUtcMs =
            (file.metadata?["recorded_at_utc_ms"] as? NSNumber)?.int64Value
            ?? Int64(Date().timeIntervalSince1970 * 1000)
        Task { @MainActor [weak self] in
            guard let self else { return }
            let destination = FileManager.default.temporaryDirectory
                .appending(path: "watch-\(UUID().uuidString).pcm")
            guard let audio = try? transcodeToCaptureFormat(
                source: source, destination: destination
            ) else { return }
            try? FileManager.default.removeItem(at: source)
            await self.enqueue(
                audio: audio,
                recordedAtUtcMs: recordedAtUtcMs,
                title: AppModel.title(
                    prefix: NSLocalizedString("title.watch", comment: ""),
                    utcMs: recordedAtUtcMs
                )
            )
        }
    }
}

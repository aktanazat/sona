import Combine
import Foundation
import Network
import PhotosUI
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

/// What the board's one status line can say about the vault.
enum BoardState: Equatable {
    case idle
    case syncing
    case offline
}

/// A photo or screenshot on its way into a thought, already on disk.
struct CapturedImage {
    var url: URL
    var mime: String
    var byteLength: Int
    var sha256: String
    var width: Int
    var height: Int
}

/// One owner of everything that outlives a screen: the vault state, the outbox, the
/// microphone, connectivity and the watch link.
@MainActor
final class AppModel: NSObject, ObservableObject {
    @Published private(set) var vault: VaultState
    @Published private(set) var queued: [QueuedObject] = []
    @Published private(set) var outbox: OutboxState = .empty
    @Published private(set) var board: BoardIndex?
    @Published private(set) var boardState: BoardState = .idle
    /// How many thoughts this run has queued; the capture screen watches it change.
    @Published private(set) var thoughtsKept = 0
    @Published private(set) var pairingOffer: PairingOffer?
    @Published var pairingMessage: LocalizedStringKey?
    @Published var endpointDraft: String
    @Published var vaultIdDraft: String
    @Published var consentAccepted: Bool
    @Published private(set) var startingRecording = false
    /// The draft the keyboard can insert, or nothing waiting for it.
    @Published private(set) var keyboardDraft: UUID?
    @Published var meetingTitle = UserDefaults.standard.string(forKey: "sona.meeting.title") ?? "" {
        didSet { UserDefaults.standard.set(meetingTitle, forKey: "sona.meeting.title") }
    }
    @Published var meetingNotes = UserDefaults.standard.string(forKey: "sona.meeting.notes") ?? "" {
        didSet { UserDefaults.standard.set(meetingNotes, forKey: "sona.meeting.notes") }
    }
    @Published private(set) var keyboardReturn = false

    let library = PhoneLibrary()
    let calendar = PhoneCalendar()
    let calls = PhoneCalls()
    private var keyboardSignal: KeyboardSignal?
    private var keyboardTicker: Task<Void, Never>?
    private var keyboardRequestID: UUID?
    private var lastKeyboardRequestID: UUID?
    private var keyboardDocumentID: UUID?
    private var pendingURL: URL?

    let recorder = PhoneRecorder()
    let dictation = PhoneDictation()
    let callOffers = CallOfferService()

    /// Set when this recording was started from a call notification.
    private var afterCall = false
    private let queue: UploadQueue
    private let reader: VaultReader
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
            root: AppModel.supportDirectory("outbox"),
            identity: state.identity,
            credentials: state.credentials
        )
        reader = VaultReader(
            root: AppModel.supportDirectory("board"),
            identity: state.identity,
            credentials: state.credentials
        )
        super.init()
        recorder.onInterrupted = { [weak self] in
            self?.stopRecording()
        }
        dictation.onEnded = { [weak self] capture in self?.collectDictation(capture) }
        dictation.onStateChanged = { [weak self] in self?.publishKeyboardState() }
        keyboardSignal = KeyboardSignal(.command) { [weak self] in
            Task { @MainActor in self?.receiveKeyboardCommand() }
        }
        publishKeyboardState()
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
            await refreshBoard()
        }
    }

    var isPaired: Bool { vault.credentials != nil }

    var deviceId: String { vault.identity.deviceId }

    func acceptConsent() {
        UserDefaults.standard.set(AppModel.consentVersion, forKey: AppModel.consentKey)
        consentAccepted = true
        /* The one notification prompt in the app, asked where its purpose is stated. */
        Task { await callOffers.requestNotifications() }
        if let pendingURL { handle(pendingURL) }
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
    var canStartDictation: Bool { consentAccepted && !recorder.isRecording && !startingRecording }

    func startDictation() {
        guard canStartDictation else { return }
        if keyboardReturn {
            guard UserDefaults.standard.bool(forKey: DictationPreferences.keyboardKey) else { return }
            startKeyboardDictation(requestID: UUID(), documentID: keyboardDocumentID)
        } else {
            dictation.start()
        }
    }

    func handle(_ url: URL) {
        pendingURL = url
        guard consentAccepted else { return }
        if url == DictationLink.meetingURL {
            pendingURL = nil
            startRecording()
        } else if DictationLink.opens(url) {
            keyboardDocumentID = DictationLink.keyboardDocument(url)
            keyboardReturn = keyboardDocumentID != nil
            guard !keyboardReturn || UserDefaults.standard.bool(forKey: DictationPreferences.keyboardKey) else { return }
            pendingURL = nil
            startDictation()
        }
    }

    func keyboardPermissionChanged(_ enabled: Bool) {
        if !enabled {
            dictation.cancel()
            dictation.endWarmSession()
            withdrawKeyboardDraft()
            keyboardRequestID = nil
            lastKeyboardRequestID = nil
        } else if let pendingURL { handle(pendingURL) }
        publishKeyboardState()
    }

    private func startKeyboardDictation(requestID: UUID, documentID: UUID?) {
        guard canStartDictation, !dictation.isBusy else { return }
        keyboardRequestID = requestID
        lastKeyboardRequestID = requestID
        keyboardDocumentID = documentID
        dictation.start(keyboard: true)
    }

    private func receiveKeyboardCommand() {
        do {
            guard let command = try KeyboardSessionStore.shared().takeCommand(),
                  UserDefaults.standard.bool(forKey: DictationPreferences.keyboardKey)
            else { return }
            switch command.action {
            case .start:
                guard dictation.isWarm else { publishKeyboardState(); return }
                startKeyboardDictation(requestID: command.requestID, documentID: command.documentID)
            case .stop:
                guard command.requestID == keyboardRequestID else { return }
                if dictation.phase == .authorizing { dictation.cancel() } else { dictation.finish() }
            case .cancel:
                guard command.requestID == keyboardRequestID else { return }
                dictation.cancel()
            }
        } catch { library.error = error.localizedDescription }
    }

    private func publishKeyboardState() {
        let phase: KeyboardSessionState.Phase
        switch dictation.phase {
        case .listening: phase = .listening
        case .authorizing, .finishing: phase = .finishing
        case .idle:
            keyboardRequestID = nil
            phase = dictation.isWarm ? .ready : (dictation.notice == nil ? .off : .failed)
        }
        do {
            try KeyboardSessionStore.shared().publish(KeyboardSessionState(
                phase: phase, expiresAt: dictation.warmUntil ?? .distantPast,
                heartbeat: Date(), requestID: keyboardRequestID ?? lastKeyboardRequestID, message: dictation.notice
            ))
        } catch { library.error = error.localizedDescription }
        if dictation.isWarm, keyboardTicker == nil {
            keyboardTicker = Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(2))
                    guard !Task.isCancelled, let self else { return }
                    if !dictation.isWarm { keyboardTicker = nil; publishKeyboardState(); return }
                    publishKeyboardState()
                }
            }
        }
    }

    func startRecording() {
        guard consentAccepted, !startingRecording, !recorder.isRecording else { return }
        /* A recording takes the microphone from dictation instead of refusing: the
         * transcript so far stays on the dictation screen, and an offer the operator
         * accepted from a call notification must not fail with a microphone error. */
        dictation.cancel()
        dictation.endWarmSession()
        keyboardReturn = false
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
        let wasAfterCall = afterCall
        afterCall = false
        let notes = meetingNotes
        let chosenTitle = meetingTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let finished = recorder.stop() else {
            /* Stopping with nothing to keep is the one outcome that must never pass in
             * silence: the operator watched a clock run and has to be told it is gone. */
            if recorder.notice == nil { recorder.notice = .notSaved }
            return
        }
        AppModel.tap()
        Task {
            let kept = await enqueue(
                audio: finished.audio,
                recordedAtUtcMs: finished.recordedAtUtcMs,
                title: chosenTitle.isEmpty ? AppModel.title(
                    prefix: wasAfterCall
                        ? NSLocalizedString("title.afterCall", comment: "")
                        : NSLocalizedString("title.phone", comment: ""),
                    utcMs: finished.recordedAtUtcMs
                ) : chosenTitle,
                notes: notes
            )
            if kept && !recorder.isRecording && meetingNotes == notes && meetingTitle.trimmingCharacters(in: .whitespacesAndNewlines) == chosenTitle {
                meetingTitle = ""
                meetingNotes = ""
            }
        }
    }

    @discardableResult
    private func enqueue(audio: CapturedAudio, recordedAtUtcMs: Int64, title: String, notes: String = "") async -> Bool {
        do {
            try library.keep(kind: .meeting, title: title, text: notes,
                             date: Date(timeIntervalSince1970: Double(recordedAtUtcMs) / 1000), audio: audio)
            _ = try await queue.enqueue(audio: audio, recordedAtUtcMs: recordedAtUtcMs, title: title, notes: notes)
            await drain()
            return true
        } catch { library.error = error.localizedDescription; return false }
    }

    @discardableResult
    func importRecording(_ url: URL, title: String? = nil, recordedAt: Date = Date()) async -> Bool {
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let destination = FileManager.default.temporaryDirectory.appending(path: "import-\(UUID().uuidString).pcm")
        do {
            let audio = try await Task.detached {
                try transcodeToCaptureFormat(source: url, destination: destination)
            }.value
            return await enqueue(audio: audio, recordedAtUtcMs: Int64(recordedAt.timeIntervalSince1970 * 1000),
                                 title: title ?? url.deletingPathExtension().lastPathComponent)
        } catch { library.error = error.localizedDescription; return false }
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
            /* Reaching the vault proves the address before a code is shown for it, and
             * corrects the phone's clock: the Mac rejects an offer whose expiry is
             * outside its own fifteen-minute window. */
            try await client.syncClock()
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
        } catch CompanionError.invalidEndpoint {
            pairingMessage = "pair.badEndpoint"
        } catch is CompanionError {
            pairingMessage = "pair.unreachable"
        } catch {
            pairingMessage = "pair.failed"
        }
    }

    /// Read the approval the desktop wrote and store the vault root it carries.
    func finishPairing() async {
        let endpoint = endpointDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var pending = vault.pending, !endpoint.isEmpty else {
            pairingMessage = "pair.needFields"
            return
        }
        /* The offer names this phone and the vault, not the address, so the approval is
         * read from the address in the field: a mistyped one is corrected there, because
         * the vault refuses a second approval for a device it already knows. */
        pending.endpoint = endpoint
        pairingMessage = nil
        do {
            let credentials = try await Pairing.finish(identity: vault.identity, pending: pending)
            var state = vault
            state.credentials = credentials
            state.pending = nil
            VaultKeychain.save(state)
            vault = state
            pairingOffer = nil
            await queue.setCredentials(credentials)
            await reader.setCredentials(credentials)
            pairingMessage = "pair.done"
            await drain()
        } catch PairingError.notApprovedYet {
            pairingMessage = "pair.notApproved"
        } catch PairingError.offerExpired {
            pairingMessage = "pair.expired"
        } catch CompanionError.invalidEndpoint {
            pairingMessage = "pair.badEndpoint"
        } catch CompanionError.transport, CompanionError.invalidResponse {
            pairingMessage = "pair.unreachable"
        } catch {
            pairingMessage = "pair.failed"
        }
    }

    // MARK: - Thoughts

    /// A voice thought is a dictation session that keeps its audio.
    func startVoiceThought() {
        guard canStartDictation else { return }
        dictation.start(keepingAudio: true)
    }

    private func collectDictation(_ capture: DictationCapture) {
        if capture.audio != nil || !capture.text.isEmpty {
            do {
                try library.keep(kind: capture.isThought ? .note : .dictation,
                                 title: String(capture.text.prefix(80)), text: capture.text,
                                 date: capture.startedAt, audio: capture.audio)
            } catch { library.error = error.localizedDescription }
        }
        if keyboardRequestID != nil, !capture.isThought, capture.error == nil, !capture.text.isEmpty {
            do {
                keyboardDraft = try KeyboardDraftStore.shared()
                    .save(text: capture.text, documentID: keyboardDocumentID).id
            } catch { library.error = error.localizedDescription }
        }
        keyboardRequestID = nil
        guard let audio = capture.audio else { return }
        guard capture.isThought else {
            try? FileManager.default.removeItem(at: audio.url)
            return
        }
        let thought = QueuedThought(
            origin: .voice, text: capture.text, link: nil,
            attachments: [ThoughtAttachment(kind: .audio(durationMs: audio.durationMs),
                file: "audio.pcm", byteLength: audio.byteLength, sha256: audio.sha256)]
        )
        AppModel.tap()
        thoughtsKept += 1
        Task {
            await enqueue(thought: thought, capturedAtUtcMs: Int64(capture.startedAt.timeIntervalSince1970 * 1000),
                          files: [audio.url])
        }
    }

    /// Typed text, with any photos already on disk. Text that is one address is a link.
    /// Returns false when there is nothing to keep.
    @discardableResult
    func captureTyped(_ text: String, images: [CapturedImage]) -> Bool {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty || !images.isEmpty else { return false }
        do {
            try library.keep(kind: .note, title: String(text.prefix(80)), text: text)
        } catch { library.error = error.localizedDescription; return false }
        let thought = QueuedThought(
            origin: .typed,
            text: text,
            link: AppModel.link(in: text),
            attachments: images.enumerated().map { index, image in
                ThoughtAttachment(
                    kind: .image(mime: image.mime, width: image.width, height: image.height),
                    file: "image-\(index).jpg",
                    byteLength: image.byteLength,
                    sha256: image.sha256
                )
            }
        )
        AppModel.tap()
        thoughtsKept += 1
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        Task { await enqueue(thought: thought, capturedAtUtcMs: now, files: images.map(\.url)) }
        return true
    }

    /// Read one picked photo into the capture format: JPEG, at most 2048 px on its long
    /// edge, on disk. Nil when the item is not an image this device can decode.
    nonisolated static func prepare(_ item: PhotosPickerItem) async -> CapturedImage? {
        guard let data = try? await item.loadTransferable(type: Data.self),
              let image = UIImage(data: data)
        else { return nil }
        let longest = max(image.size.width, image.size.height)
        let scale = min(1, 2048 / max(longest, 1))
        let size = CGSize(
            width: (image.size.width * scale).rounded(.down),
            height: (image.size.height * scale).rounded(.down)
        )
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let jpeg = UIGraphicsImageRenderer(size: size, format: format).jpegData(
            withCompressionQuality: 0.85
        ) { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }
        let url = FileManager.default.temporaryDirectory
            .appending(path: "thought-image-\(UUID().uuidString).jpg")
        guard (try? jpeg.write(to: url, options: .atomic)) != nil else { return nil }
        return CapturedImage(
            url: url,
            mime: "image/jpeg",
            byteLength: jpeg.count,
            sha256: sha256Base64URL(jpeg),
            width: Int(size.width),
            height: Int(size.height)
        )
    }

    /// The one address `text` is, or nil when it is prose.
    static func link(in text: String) -> String? {
        guard !text.contains(where: \.isWhitespace),
              let url = URL(string: text),
              let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = url.host, !host.isEmpty
        else { return nil }
        return text
    }

    private func enqueue(thought: QueuedThought, capturedAtUtcMs: Int64, files: [URL]) async {
        _ = try? await queue.enqueue(
            thought: thought, capturedAtUtcMs: capturedAtUtcMs, files: files
        )
        await drain()
    }

    // MARK: - Board

    /// Fold what the vault has written since the last look. The board keeps what it had
    /// through a failed sync; only the status line says the vault was out of reach.
    func refreshBoard() async {
        guard isPaired, boardState != .syncing else { return }
        if let current = await reader.current() { dictation.profile = current.dictationProfile ?? .empty }
        boardState = .syncing
        if board == nil { board = await reader.current() }
        do {
            board = try await reader.sync()
            dictation.profile = board?.dictationProfile ?? .empty
            boardState = .idle
        } catch {
            boardState = .offline
        }
    }

    /// Remove a thought and every card written about it, from the vault and the board.
    func deleteThought(id: String) async {
        do {
            try await reader.deleteThought(id)
            board = await reader.current()
        } catch {
            boardState = .offline
        }
    }

    /// One image attachment's bytes, from the on-disk copy or the vault.
    func image(objectId: String, head: BoardHead, image: ThoughtManifest.Image) async -> UIImage? {
        let cached = AppModel.supportDirectory("images").appending(path: "\(image.sha256).jpg")
        if let bytes = try? Data(contentsOf: cached) {
            return UIImage(data: bytes)
        }
        guard let bytes = try? await reader.attachment(
            objectId: objectId,
            head: head,
            chunkStart: image.chunk_start,
            chunkCount: image.chunk_count,
            sha256: image.sha256
        ) else { return nil }
        try? bytes.write(to: cached, options: .atomic)
        return UIImage(data: bytes)
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

    /// One directory under Application Support, made on first use.
    private static func supportDirectory(_ name: String) -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? FileManager.default.temporaryDirectory
        let directory = base.appending(path: name)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
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

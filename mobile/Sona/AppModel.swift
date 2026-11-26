import ActivityKit
import Combine
import Foundation
import ImageIO
import Network
import PhotosUI
import Speech
import SwiftUI
import UIKit
import UserNotifications
import WatchConnectivity
import WidgetKit

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

enum AppPage { case board, capture, recording, dictation, library }

/// Where Sona is in allowing the keyboard its microphone.
enum KeyboardSetup: Equatable {
    case asking, ready
    case refused(String)
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
    @Published var page = AppPage.capture
    /// The draft the keyboard can insert, or nothing waiting for it.
    @Published private(set) var keyboardDraft: UUID?
    @Published var meetingTitle = UserDefaults.standard.string(forKey: "sona.meeting.title") ?? "" {
        didSet { UserDefaults.standard.set(meetingTitle, forKey: "sona.meeting.title") }
    }
    @Published var meetingNotes = UserDefaults.standard.string(forKey: "sona.meeting.notes") ?? "" {
        didSet { UserDefaults.standard.set(meetingNotes, forKey: "sona.meeting.notes") }
    }
    @Published private(set) var keyboardReturn = false
    /// Set while the keyboard has Sona open only to allow the microphone.
    @Published private(set) var keyboardSetup: KeyboardSetup?
    private var importingKeyboardDictations = false
    /// The library note Spotlight, Siri or a shortcut asked to open.
    @Published var openedNote: UUID?
    /// A search Siri or a shortcut asked the library to show, until the library takes it.
    @Published var librarySearchRequest: String?

    let library = PhoneLibrary()
    let calendar = PhoneCalendar()
    let calls = PhoneCalls()
    private var spotlight: PhoneNoteSpotlight?
    private var keyboardSignal: KeyboardSignal?
    private var keyboardDraftSignal: KeyboardSignal?
    private var keyboardDraftExpiry: Timer?
    private var keyboardTicker: Task<Void, Never>?
    private var keyboardMeter: AnyCancellable?
    private var keyboardLevels: AsyncStream<KeyboardLevel>.Continuation?
    private var draining = false
    private var drainAgain = false
    private var uploadRetryActive = false
    private var uploadRetryTimer: Timer?
    private var keyboardRequest: (id: UUID, documentID: UUID?)?
    private var lastKeyboardRequestID: UUID?
    private var pendingURL: URL?

    let recorder = PhoneRecorder()
    private var recoveringRecordings = false
    private var savingCaptureIDs: Set<UUID> = []
    let dictation = PhoneDictation()
    let callOffers = CallOfferService()
    private let microphoneActivity = MicrophoneLiveActivity()
    /// The chat with Sona's agent. The Mac answers it through the vault.
    private(set) lazy var chat: PhoneAgentChat = {
        let chat = PhoneAgentChat(transport: self, file: AppModel.supportDirectory("agent").appending(path: "chat.json"))
        chat.onSettled = { [weak self] conversation, turn in self?.notify(conversation, turn) }
        return chat
    }()
    @Published var showsChat = false
    /// The conversation a notification or the Action Button asked the chat to show.
    @Published var chatConversation: UUID?
    /// The Mac's Today, and the decisions on their way to it.
    private(set) lazy var today = PhoneToday(
        transport: self, file: AppModel.supportDirectory("today").appending(path: "decisions.json")
    )
    /// The Board opens Today when a widget or a link asks.
    @Published var showsToday = false

    private var recordingAfterCall = false
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
        spotlight = PhoneNoteSpotlight(
            library: library, record: AppModel.supportDirectory("search").appending(path: "indexed.json")
        )
        recorder.onInterrupted = { [weak self] in
            self?.stopRecording()
        }
        DictationPreferences.moveKeyboardSwitchToSharedContainer()
        DictationPreferences.moveStyleToSharedContainer()
        dictation.onEnded = { [weak self] capture in self?.collectDictation(capture) }
        dictation.onStateChanged = { [weak self] in self?.publishKeyboardState() }
        keyboardMeter = dictation.meter.$levels.sink { [weak self] levels in
            guard let self, dictation.phase == .listening,
                  let requestID = keyboardRequest?.id, let level = levels.last
            else { return }
            if keyboardLevels == nil { keyboardLevels = KeyboardSessionStore.levelPublisher() }
            keyboardLevels?.yield(KeyboardLevel(
                requestID: requestID, uptime: ProcessInfo.processInfo.systemUptime, level: level
            ))
        }
        keyboardSignal = KeyboardSignal(.command) { [weak self] in
            Task { @MainActor in self?.receiveKeyboardCommand() }
        }
        keyboardDraftSignal = KeyboardSignal(.draftConsumed) { [weak self] in
            Task { @MainActor in
                guard let self, self.keyboardDraft != nil else { return }
                self.refreshKeyboardDraft()
            }
        }
        publishKeyboardState()
        callOffers.onOfferAccepted = { [weak self] in
            self?.startRecording(during: nil, afterCall: true)
        }
        callOffers.onAgentNotification = { [weak self] conversation in self?.openChat(conversation) }
        startConnectivityWatch()
        activateWatchSession()
        /* A launch is the operator asking again: anything the vault refused earlier gets
         * one more attempt before it is reported as still here. */
        importKeyboardDictations()
        Task {
            await recoverRecordings()
            await queue.retryParked()
            await drain()
            await refreshBoard()
            chat.watch()
        }
    }

    var isPaired: Bool { vault.credentials != nil }

    var deviceId: String { vault.identity.deviceId }

    func acceptConsent() {
        UserDefaults.standard.set(AppModel.consentVersion, forKey: AppModel.consentKey)
        consentAccepted = true
        /* The one notification prompt in the app, asked where its purpose is stated. */
        Task {
            await callOffers.requestNotifications()
            await recoverRecordings()
        }
        if let pendingURL { handle(pendingURL) }
    }

    var offersAfterCalls: Bool {
        get { callOffers.isEnabled }
        set {
            callOffers.isEnabled = newValue
            objectWillChange.send()
        }
    }

    var notesInSearch: Bool {
        get { PhoneNoteSpotlight.isEnabled }
        set {
            spotlight?.setEnabled(newValue)
            objectWillChange.send()
        }
    }

    // MARK: - Recording

    /// False while the microphone belongs to a recording, so the dictation screen can say
    /// so before the operator taps.
    var canStartDictation: Bool { consentAccepted && !recorder.isRecording && !startingRecording }

    func startDictation() {
        guard canStartDictation, !dictation.isBusy else { return }
        keyboardSetup = nil
        keyboardReturn = false
        if let pendingURL, DictationLink.fromKeyboard(pendingURL) { self.pendingURL = nil }
        lastKeyboardRequestID = nil
        dictation.start()
    }

    func handle(_ url: URL) {
        if url == DictationLink.askURL { openChat(nil); return }
        if url == WorkTodayGlance.url { page = .board; showsToday = true; return }
        if DictationLink.opens(url) { page = .dictation }
        if url == DictationLink.meetingURL { page = .recording }
        pendingURL = url
        guard consentAccepted else { return }
        if url == DictationLink.meetingURL {
            pendingURL = nil
            startRecording()
        } else if DictationLink.keyboardSetup(url) {
            keyboardReturn = false
            keyboardSetup = .asking
            guard DictationPreferences.keyboardEnabled else { return }
            pendingURL = nil
            allowKeyboardMicrophone()
        } else if DictationLink.opens(url) {
            keyboardReturn = DictationLink.fromKeyboard(url)
            guard !keyboardReturn || DictationPreferences.keyboardEnabled else { return }
            pendingURL = nil
            if keyboardReturn {
                startKeyboardDictation(
                    requestID: UUID(), documentID: DictationLink.keyboardDocument(url),
                    language: DictationLink.keyboardLanguage(url)
                )
            } else {
                startDictation()
            }
        }
    }

    /// A keyboard cannot ask for the microphone or speech recognition; Sona asks, and the
    /// keyboard then uses what was allowed.
    private func allowKeyboardMicrophone() {
        Task {
            defer { publishKeyboardState() }
            let microphone = PhoneRecorder.hasPermission ? true : await PhoneRecorder.requestPermission()
            guard microphone else {
                keyboardSetup = .refused(NSLocalizedString("status.microphoneOff", comment: ""))
                return
            }
            let speech = await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
            }
            keyboardSetup = speech ? .ready : .refused(NSLocalizedString("dictation.speechOff", comment: ""))
        }
    }

    func keyboardPermissionChanged(_ enabled: Bool) {
        if !enabled {
            dictation.cancel()
            dictation.endWarmSession()
            withdrawKeyboardDraft()
            keyboardRequest = nil
            lastKeyboardRequestID = nil
        } else if let pendingURL { handle(pendingURL) }
        publishKeyboardState()
    }

    private func startKeyboardDictation(requestID: UUID, documentID: UUID?, language: TypingLanguage?,
                                        style: String? = nil, fromWarmKeyboard: Bool = false) {
        guard canStartDictation, !dictation.isBusy else { return }
        keyboardSetup = nil
        keyboardRequest = (requestID, documentID)
        lastKeyboardRequestID = requestID
        dictation.start(keyboard: true, language: language?.rawValue, style: style,
                        warmStyleRequestID: fromWarmKeyboard ? requestID : nil)
    }

    private func receiveKeyboardCommand() {
        do {
            guard let command = try KeyboardSessionStore.shared().takeCommand(),
                  DictationPreferences.keyboardEnabled
            else { return }
            switch command.action {
            case .start:
                guard dictation.isWarm else { publishKeyboardState(); return }
                startKeyboardDictation(
                    requestID: command.requestID, documentID: command.documentID,
                    language: command.language.flatMap(TypingLanguage.init(rawValue:)), style: command.style,
                    fromWarmKeyboard: true
                )
            case .stop:
                guard command.requestID == keyboardRequest?.id else { return }
                if dictation.phase == .authorizing { dictation.cancel() } else { dictation.finish() }
            case .cancel:
                guard command.requestID == keyboardRequest?.id else { return }
                dictation.cancel()
            }
        } catch { library.error = error.localizedDescription }
    }

    private func publishKeyboardState() {
        DictationPreferences.keyboardMicrophoneAllowed =
            PhoneRecorder.hasPermission && SFSpeechRecognizer.authorizationStatus() == .authorized
        let phase: KeyboardSessionState.Phase
        switch dictation.phase {
        case .listening: phase = .listening
        case .authorizing, .finishing: phase = .finishing
        case .idle:
            keyboardRequest = nil
            phase = dictation.isWarm ? .ready : (dictation.notice == nil ? .off : .failed)
        }
        if phase != .listening {
            keyboardLevels?.finish()
            keyboardLevels = nil
        }
        do {
            try KeyboardSessionStore.shared().publish(KeyboardSessionState(
                phase: phase, expiresAt: dictation.warmUntil ?? .distantPast,
                heartbeat: Date(), requestID: keyboardRequest?.id ?? lastKeyboardRequestID, message: dictation.notice,
                heard: keyboardRequest == nil ? nil : KeyboardSessionState.heard(dictation.text)
            ))
        } catch { library.error = error.localizedDescription }
        microphoneActivity.show(dictation.isWarm ? MicrophoneActivityAttributes.ContentState(
            readyUntil: dictation.warmUntil ?? Date(), listening: dictation.phase == .listening
        ) : nil)
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

    func endMicrophoneSession() {
        dictation.endWarmSession()
    }

    func open(note id: UUID) {
        page = .library
        openedNote = id
    }

    func search(library term: String) {
        page = .library
        librarySearchRequest = term
    }

    /// The calendar event the running recording was started from.
    private var recordingEvent: DeviceRecordingManifest.CalendarEvent?
    /// Where the phone was as the running recording started, once read.
    private var recordingPlace: DeviceRecordingManifest.PlaceReading?
    private var placeRead: Task<Void, Never>?
    private let location = LocationOnce()

    func startRecording(during event: DeviceRecordingManifest.CalendarEvent? = nil) {
        startRecording(during: event, afterCall: false)
    }

    private func startRecording(during event: DeviceRecordingManifest.CalendarEvent?, afterCall: Bool) {
        guard consentAccepted, !startingRecording, !recorder.isRecording else { return }
        takeMicrophoneFromDictation()
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
            beginRecording(during: event, afterCall: afterCall)
        }
    }

    /// Reads where the phone is while Sona is open on screen, unless the
    /// invite already pins the place. A reading that comes after the stop is
    /// dropped.
    private func readPlace(unless event: DeviceRecordingManifest.CalendarEvent?) {
        placeRead?.cancel()
        recordingPlace = nil
        guard event?.place == nil, UIApplication.shared.applicationState == .active else {
            placeRead = nil
            return
        }
        placeRead = Task { [weak self] in
            guard let reading = await self?.location.read(), !Task.isCancelled else { return }
            self?.recordingPlace = DeviceRecordingManifest.PlaceReading(reading)
        }
    }

    var isRecording: Bool { startingRecording || recorder.isRecording }

    /// A press outside the app records without bringing it up only when nothing has to
    /// be asked first. The system ends a recording started that way unless a Live
    /// Activity shows it, so Live Activities have to be on too.
    func recordWithoutAsking() -> Bool {
        if isRecording { return true }
        guard consentAccepted, !startingRecording, PhoneRecorder.hasPermission,
              ActivityAuthorizationInfo().areActivitiesEnabled
        else { return false }
        takeMicrophoneFromDictation()
        guard beginRecording(during: nil, requiringActivity: true) else { return false }
        page = .recording
        return true
    }

    /// The system ends Sona when a recording intent leaves the microphone on without a
    /// Live Activity, so with Live Activities off a press only opens the record screen.
    func openRecording() {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else {
            page = .recording
            return
        }
        handle(DictationLink.meetingURL)
    }

    func bookmarkRecording() {
        recorder.bookmark()
    }

    /* A recording takes the microphone from dictation instead of refusing: the
     * transcript so far stays on the dictation screen, and an offer the operator
     * accepted from a call notification must not fail with a microphone error. */
    private func takeMicrophoneFromDictation() {
        dictation.cancel()
        dictation.endWarmSession()
        keyboardReturn = false
    }

    @discardableResult
    private func beginRecording(
        during event: DeviceRecordingManifest.CalendarEvent?,
        afterCall: Bool = false,
        requiringActivity: Bool = false
    ) -> Bool {
        do {
            try recorder.start(requiringActivity: requiringActivity)
            UserDefaults.standard.set(recorder.mostRecentCaptureID?.uuidString, forKey: Self.draftCaptureKey)
            recordingAfterCall = afterCall
            recordingEvent = event
            readPlace(unless: event)
            AppModel.tap()
            return true
        } catch is LiveActivityRefused {
            return false
        } catch {
            recorder.notice = .microphoneUnavailable
            return false
        }
    }

    // MARK: - Keyboard draft

    func approveForKeyboard(_ text: String) throws {
        trackKeyboardDraft(try KeyboardDraftStore.shared().save(text: text))
    }

    /// Withdraw what the keyboard has not taken yet. Already inserted is the same outcome.
    func withdrawKeyboardDraft() {
        guard let id = keyboardDraft else { return }
        trackKeyboardDraft(nil)
        try? KeyboardDraftStore.shared().discard(id: id)
    }

    func refreshKeyboardDraft() {
        guard consentAccepted else { return }
        if case .waiting(let draft)? = try? KeyboardDraftStore.shared().load() {
            trackKeyboardDraft(draft)
        } else {
            trackKeyboardDraft(nil)
        }
    }

    private func trackKeyboardDraft(_ draft: KeyboardDraft?) {
        keyboardDraftExpiry?.invalidate()
        keyboardDraftExpiry = nil
        if keyboardDraft != draft?.id { keyboardDraft = draft?.id }
        guard let draft else { return }
        let id = draft.id
        let expiry = draft.createdAt.addingTimeInterval(KeyboardDraftStore.lifetime)
        let timer = Timer(fire: expiry, interval: 0, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.keyboardDraft == id else { return }
                self.refreshKeyboardDraft()
            }
        }
        keyboardDraftExpiry = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    func stopRecording() {
        guard recorder.isRecording else { return }
        let wasAfterCall = recordingAfterCall
        recordingAfterCall = false
        let notes = meetingNotes
        let chosenTitle = meetingTitle.trimmingCharacters(in: .whitespacesAndNewlines)
        let event = recordingEvent
        recordingEvent = nil
        let place = recordingPlace
        recordingPlace = nil
        placeRead?.cancel()
        placeRead = nil
        guard let finished = recorder.stop() else {
            /* Stopping with nothing to keep is the one outcome that must never pass in
             * silence: the operator watched a clock run and has to be told it is gone. */
            if recorder.notice == nil { recorder.notice = .notSaved }
            return
        }
        AppModel.tap()
        /* A stop pressed with the phone locked has turned the audio session off, so
         * nothing else keeps Sona running until the recording is queued and its upload
         * has had its chance. An upload cut off when the time runs out resumes later. */
        var saving = UIBackgroundTaskIdentifier.invalid
        let release = {
            if saving != .invalid { UIApplication.shared.endBackgroundTask(saving) }
            saving = .invalid
        }
        saving = UIApplication.shared.beginBackgroundTask(withName: "Save recording") { release() }
        Task {
            let kept: Bool
            /* A save that threw left the audio on the iPhone; one that found no audio
             * removed the capture, so there is nothing to try again. */
            var failedToSave = false
            do {
                kept = try await keepCapture(
                    finished.journal, audio: finished.audio,
                    title: chosenTitle.isEmpty ? AppModel.title(
                        prefix: wasAfterCall
                            ? NSLocalizedString("title.afterCall", comment: "")
                            : NSLocalizedString("title.phone", comment: ""),
                        utcMs: finished.journal.recordedAtUtcMs
                    ) : chosenTitle,
                    notes: notes, calendarEvent: event, placeReading: place
                )
            } catch {
                library.error = error.localizedDescription
                kept = false
                failedToSave = true
            }
            if failedToSave { recorder.retryLater(finished.journal.id) }
            if !kept, !recorder.isRecording, recorder.mostRecentCaptureID == finished.journal.id, recorder.notice == nil {
                recorder.notice = failedToSave ? .keptForRetry : .notSaved
            }
            await drain()
            release()
            if kept && !recorder.isRecording && recorder.mostRecentCaptureID == finished.journal.id
                && meetingNotes == notes && meetingTitle.trimmingCharacters(in: .whitespacesAndNewlines) == chosenTitle {
                meetingTitle = ""
                meetingNotes = ""
                UserDefaults.standard.removeObject(forKey: Self.draftCaptureKey)
            }
        }
    }

    /// The recording the title and notes being typed belong to, so a recording recovered
    /// after the app was killed takes them instead of leaving them for the next one.
    private static let draftCaptureKey = "sona.meeting.captureID"

    func recoverRecordings() async {
        guard consentAccepted, UIApplication.shared.isProtectedDataAvailable, !recoveringRecordings else { return }
        recoveringRecordings = true
        defer { recoveringRecordings = false }
        let root = recorder.captureRoot
        var queuedRecovery = false
        do {
            let folders = try await Task.detached { try PhoneCaptureJournal.pending(under: root) }.value
            for folder in folders {
                guard let id = UUID(uuidString: folder.lastPathComponent),
                      !recorder.capturesStartedThisRun.contains(id)
                else { continue }
                do {
                    let journal = try await Task.detached { try PhoneCaptureJournal.load(from: folder) }.value
                    let ownsDraft = UserDefaults.standard.string(forKey: Self.draftCaptureKey) == id.uuidString
                    let typed = ownsDraft ? meetingTitle.trimmingCharacters(in: .whitespacesAndNewlines) : ""
                    let title = typed.isEmpty
                        ? AppModel.title(prefix: NSLocalizedString("title.recoveredRecording", comment: ""),
                                         utcMs: journal.recordedAtUtcMs)
                        : typed
                    let notes = ownsDraft ? meetingNotes : ""
                    if try await keepCapture(journal, audio: nil, title: title, notes: notes) {
                        queuedRecovery = true
                        if ownsDraft {
                            meetingTitle = ""
                            meetingNotes = ""
                            UserDefaults.standard.removeObject(forKey: Self.draftCaptureKey)
                        }
                    }
                } catch { library.error = error.localizedDescription }
            }
        } catch { library.error = error.localizedDescription }
        if queuedRecovery { await drain() }
    }

    private func keepCapture(
        _ journal: PhoneCaptureJournal, audio: CapturedAudio?, title: String, notes: String = "",
        calendarEvent: DeviceRecordingManifest.CalendarEvent? = nil,
        placeReading: DeviceRecordingManifest.PlaceReading? = nil
    ) async throws -> Bool {
        savingCaptureIDs.insert(journal.id)
        defer { savingCaptureIDs.remove(journal.id) }
        let root = recorder.captureRoot
        let directory = journal.directory(under: root)
        let captured: CapturedAudio?
        if let audio, audio.byteLength > 0 {
            captured = audio
        } else {
            captured = try await Task.detached { try journal.recoverAudio(under: root) }.value
        }
        guard let captured else {
            try Self.removePendingCapture(at: directory)
            return false
        }
        let date = Date(timeIntervalSince1970: Double(journal.recordedAtUtcMs) / 1000)
        let note = try await library.keepRecording(id: journal.id, title: title, notes: notes, date: date, audio: captured)
        _ = try await queue.enqueue(
            audio: captured, recordedAtUtcMs: journal.recordedAtUtcMs, title: note.title, notes: note.text,
            calendarEvent: calendarEvent, bookmarkOffsetsMs: journal.bookmarkOffsetsMs.filter { $0 <= captured.durationMs },
            placeReading: placeReading, captureDirectory: directory
        )
        return true
    }

    func deleteLocalNote(_ note: PhoneNote) {
        guard !savingCaptureIDs.contains(note.id) else {
            library.error = NSLocalizedString("library.recordingStillSaving", comment: "")
            return
        }
        do {
            if note.kind == .meeting {
                try Self.removePendingCapture(at: recorder.captureRoot.appending(path: note.id.uuidString))
            }
            try library.delete(note)
        } catch {
            library.error = error.localizedDescription
        }
    }

    private static func removePendingCapture(at directory: URL) throws {
        do { try FileManager.default.removeItem(at: directory) }
        catch let error as CocoaError where error.code == .fileNoSuchFile || error.code == .fileReadNoSuchFile {}
    }

    @discardableResult
    private func enqueue(
        audio: CapturedAudio,
        recordedAtUtcMs: Int64,
        title: String,
        notes: String = "",
        calendarEvent: DeviceRecordingManifest.CalendarEvent? = nil,
        bookmarkOffsetsMs: [Int64] = [],
        placeReading: DeviceRecordingManifest.PlaceReading? = nil
    ) async -> Bool {
        do {
            try library.keep(kind: .meeting, title: title, text: notes,
                             date: Date(timeIntervalSince1970: Double(recordedAtUtcMs) / 1000), audio: audio)
            _ = try await queue.enqueue(
                audio: audio, recordedAtUtcMs: recordedAtUtcMs, title: title, notes: notes,
                calendarEvent: calendarEvent, bookmarkOffsetsMs: bookmarkOffsetsMs,
                placeReading: placeReading
            )
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
            let kept = await enqueue(audio: audio, recordedAtUtcMs: Int64(recordedAt.timeIntervalSince1970 * 1000),
                                     title: title ?? url.deletingPathExtension().lastPathComponent)
            await drain()
            return kept
        } catch { library.error = error.localizedDescription; return false }
    }

    /// Run the outbox and report where it ended up, so the one status line can say
    /// "uploading", "waiting for a connection" or "saved" without guessing. One run at a
    /// time: a call during a run makes it go round once more.
    private func drain() async {
        guard !draining else {
            drainAgain = true
            return
        }
        uploadRetryTimer?.invalidate()
        uploadRetryTimer = nil
        draining = true
        /* The pass holds the time iOS gives a backgrounded app, so a recording queued while
         * another pass runs (a stop on a locked phone, a watch file) is sent before suspension:
         * callers that find a pass running return at once and rely on this one. */
        var working = UIBackgroundTaskIdentifier.invalid
        let release = {
            if working != .invalid { UIApplication.shared.endBackgroundTask(working) }
            working = .invalid
        }
        working = UIApplication.shared.beginBackgroundTask(withName: "Upload recordings") { release() }
        defer {
            draining = false
            release()
        }
        var retryCutoff: Date?
        repeat {
            drainAgain = false
            queued = await queue.items()
            guard !queued.isEmpty else {
                outbox = .empty
                continue
            }
            outbox = .uploading
            // Keep a deadline that passes between the final queue check and this callback.
            retryCutoff = Date()
            await queue.drain()
            queued = await queue.items()
            outbox = queued.isEmpty ? .saved : .waiting
        } while drainAgain
        if let retryCutoff { scheduleUploadRetry(after: retryCutoff) }
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

    func setUploadRetryActive(_ active: Bool) {
        uploadRetryActive = active
        if active {
            drainNow()
        } else {
            uploadRetryTimer?.invalidate()
            uploadRetryTimer = nil
        }
    }

    private func scheduleUploadRetry(after cutoff: Date) {
        guard uploadRetryActive, vault.credentials != nil,
              let deadline = QueuedObject.nextRetry(in: queued, after: cutoff)
        else { return }
        let timer = Timer(fire: deadline, interval: 0, repeats: false) { [weak self] timer in
            MainActor.assumeIsolated {
                guard let self, self.uploadRetryTimer === timer else { return }
                self.drainNow()
            }
        }
        uploadRetryTimer = timer
        RunLoop.main.add(timer, forMode: .common)
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

    /// Dictations the keyboard heard itself wait in the shared container until Sona keeps
    /// them in Notes, with their audio, as it keeps its own.
    func importKeyboardDictations() {
        guard !importingKeyboardDictations, let queue = try? KeyboardDictationQueue.shared() else { return }
        importingKeyboardDictations = true
        Task {
            defer { importingKeyboardDictations = false }
            await library.importKeyboardDictations(from: queue)
        }
    }

    private func collectDictation(_ capture: DictationCapture) {
        var keyboardStore: KeyboardDraftStore?
        var draftError: String?
        if keyboardRequest != nil, !capture.isThought, capture.error == nil, !capture.text.isEmpty {
            do { keyboardStore = try KeyboardDraftStore.shared() }
            catch { draftError = error.localizedDescription }
        }
        if !capture.isThought, capture.error == nil {
            var stats = DictationStats.load()
            stats.record(capture.text, seconds: capture.audio.map { Double($0.durationMs) / 1000 }
                ?? Date().timeIntervalSince(capture.startedAt))
            stats.save()
        }
        let background = UIApplication.shared.beginBackgroundTask(withName: "Save dictation")
        let delivery = library.deliverDictation(
            kind: capture.isThought ? .note : .dictation,
            text: capture.text, date: capture.startedAt, audio: capture.audio,
            keyboardStore: keyboardStore, documentID: keyboardRequest?.documentID,
            removeSourceAfterSaving: !capture.isThought
        )
        if let draft = delivery.draft { trackKeyboardDraft(draft) }
        if let draftError { library.error = draftError }
        keyboardRequest = nil
        Task {
            defer {
                if background != .invalid { UIApplication.shared.endBackgroundTask(background) }
            }
            _ = await delivery.saving.value
            if let draftError { library.error = draftError }
            guard capture.isThought, let audio = capture.audio else { return }
            let thought = QueuedThought(
                origin: .voice, text: capture.text, link: nil,
                attachments: [ThoughtAttachment(kind: .audio(durationMs: audio.durationMs),
                    file: "audio.pcm", byteLength: audio.byteLength, sha256: audio.sha256)]
            )
            AppModel.tap()
            thoughtsKept += 1
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
    /// edge, on disk. Nil when the item is not an image this device can decode. The photo
    /// is decoded straight at that size; a camera frame decoded whole first takes about
    /// 190 MB at 48 megapixels.
    nonisolated static func prepare(_ item: PhotosPickerItem) async -> CapturedImage? {
        guard let data = try? await item.loadTransferable(type: Data.self),
              let source = CGImageSourceCreateWithData(data as CFData, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                  kCGImageSourceCreateThumbnailFromImageAlways: true,
                  kCGImageSourceCreateThumbnailWithTransform: true,
                  kCGImageSourceThumbnailMaxPixelSize: 2048,
              ] as CFDictionary),
              let jpeg = UIImage(cgImage: image).jpegData(compressionQuality: 0.85)
        else { return nil }
        let url = FileManager.default.temporaryDirectory
            .appending(path: "thought-image-\(UUID().uuidString).jpg")
        guard (try? jpeg.write(to: url, options: .atomic)) != nil else { return nil }
        return CapturedImage(
            url: url,
            mime: "image/jpeg",
            byteLength: jpeg.count,
            sha256: sha256Base64URL(jpeg),
            width: image.width,
            height: image.height
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
        if let current = await reader.current() { adopt(current.dictationProfile ?? .empty) }
        boardState = .syncing
        if board == nil { board = await reader.current() }
        do {
            board = try await reader.sync()
            adopt(board?.dictationProfile ?? .empty)
            boardState = .idle
            await settleToday()
        } catch {
            boardState = .offline
        }
    }

    /// Reads what the Mac's newest Today says about the decisions on their way, and hands
    /// the widgets a fresh copy when it changed.
    func settleToday() async {
        let snapshot = board?.workToday
        await today.reconcile(snapshot)
        let glance = snapshot.map(WorkTodayGlance.init)
        guard glance != WorkTodayGlance.load() else { return }
        if let glance { try? glance.save() } else { WorkTodayGlance.remove() }
        WidgetCenter.shared.reloadTimelines(ofKind: WorkTodayGlance.widgetKind)
    }

    /// Sends one decision about the Mac's work and starts it on its way.
    func decide(
        _ action: WorkActionManifest.Action, row: String, title: String, taskId: String? = nil,
        expectedRevision: Int64? = nil, noticeIds: [String] = [], sessionId: String? = nil,
        askedAtMs: Int64? = nil, body: String? = nil, value: String? = nil
    ) async {
        guard let snapshot = board?.workToday else { return }
        do {
            try await today.decide(
                action, row: row, title: title, in: snapshot, deviceId: deviceId,
                taskId: taskId, expectedRevision: expectedRevision, noticeIds: noticeIds,
                sessionId: sessionId, askedAtMs: askedAtMs, body: body, value: value
            )
        } catch {
            boardState = .offline
        }
    }

    /// The keyboard keeps the vocabulary's written forms as typed, so they reach the app
    /// group whenever the profile arrives from the Mac.
    private func adopt(_ profile: DictationProfile) {
        dictation.profile = profile
        try? KeyboardVocabularyStore.shared().save(profile)
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
            byteLength: image.byte_length,
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

extension AppModel: TodayTransport {
    func send(_ action: WorkActionManifest) async throws -> String {
        guard isPaired else { throw UploadQueueError.notPaired }
        let item = try await queue.enqueue(workAction: action)
        Task { await drain() }
        return item.objectId
    }

    func deleteObject(_ objectId: String) async throws {
        try await reader.deleteAgentObject(objectId)
        board = await reader.current()
    }
}

extension AppModel: AgentTransport {
    func send(_ ask: QueuedAsk, askedAt: Date) async throws -> String {
        guard isPaired else { throw UploadQueueError.notPaired }
        let item = try await queue.enqueue(ask: ask, askedAtUtcMs: Int64(askedAt.timeIntervalSince1970 * 1000))
        Task { await drain() }
        return item.objectId
    }

    func outbox(_ askObjectId: String) async -> AskOutbox {
        await queue.state(of: askObjectId)
    }

    func withdraw(_ askObjectId: String) async -> Bool {
        let taken = await queue.withdraw(objectId: askObjectId)
        await refresh()
        return taken
    }

    func replies() async throws -> [String: AgentReplyManifest] {
        let index = try await reader.sync()
        board = index
        return index.agentReplies
    }

    func deleteAsk(_ askObjectId: String) async throws {
        try await reader.deleteAgentObject(askObjectId)
        board = await reader.current()
    }

    func deleteReply(_ askObjectId: String) async throws {
        try await reader.deleteAgentObject(AgentAskObject.replyObjectId(askObjectId: askObjectId))
        board = await reader.current()
    }

    /// An answer, a failure or an overdue question becomes a notification while Sona is
    /// not on screen.
    func notify(_ conversation: AgentConversation, _ turn: AgentTurn) {
        guard UIApplication.shared.applicationState != .active else { return }
        let content = UNMutableNotificationContent()
        content.threadIdentifier = conversation.id.uuidString
        content.userInfo = [AgentNotification.conversationKey: conversation.id.uuidString]
        content.sound = .default
        if turn.role == .assistant {
            content.title = NSLocalizedString("agent.notification.answered", comment: "")
            content.subtitle = conversation.turns.last { $0.role == .user && $0.askObjectId == turn.askObjectId }?.text ?? ""
            content.body = turn.text
        } else {
            content.title = NSLocalizedString(
                turn.status == .unanswered ? "agent.notification.waiting" : "agent.notification.failed", comment: ""
            )
            content.subtitle = turn.text
            content.body = NSLocalizedString(AgentFailure.message(turn.status), comment: "")
        }
        UNUserNotificationCenter.current().add(UNNotificationRequest(
            identifier: AgentNotification.identifierPrefix + turn.id.uuidString, content: content, trigger: nil
        ))
    }

    func openChat(_ conversation: UUID?) {
        chatConversation = conversation
        showsChat = true
    }
}

extension AppModel: PhoneRecordingControls {}
extension AppModel: PhoneMicrophoneControls {}

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
        let destination = FileManager.default.temporaryDirectory
            .appending(path: "watch-\(UUID().uuidString).pcm")
        Task { @MainActor [weak self] in
            guard let self else { return }
            /* The watch's file can arrive with Sona in the background, which would be
             * suspended before the recording is converted, queued and uploaded. */
            var working = UIBackgroundTaskIdentifier.invalid
            let release = {
                if working != .invalid { UIApplication.shared.endBackgroundTask(working) }
                working = .invalid
            }
            working = UIApplication.shared.beginBackgroundTask(withName: "Save watch recording") { release() }
            defer { release() }
            guard let audio = try? await Task.detached(priority: .utility, operation: {
                try transcodeToCaptureFormat(source: source, destination: destination)
            }).value else { return }
            try? FileManager.default.removeItem(at: source)
            let kept = await self.enqueue(
                audio: audio,
                recordedAtUtcMs: recordedAtUtcMs,
                title: AppModel.title(
                    prefix: NSLocalizedString("title.watch", comment: ""),
                    utcMs: recordedAtUtcMs
                )
            )
            if kept { await self.drain() }
        }
    }
}

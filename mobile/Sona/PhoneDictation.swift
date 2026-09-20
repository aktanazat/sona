import AVFoundation
import Combine
import Foundation
import Speech

/// Dictation stays on this phone. The keyboard never owns microphone access.
@MainActor
final class PhoneDictation: ObservableObject {
    enum Phase {
        case idle
        case authorizing
        case listening
        case finishing
    }

    @Published private(set) var phase: Phase = .idle
    @Published var text = ""
    @Published private(set) var notice: String?
    /// Runs when a session ends, however it ended, after `phase` is idle again.
    var onEnded: (() -> Void)?
    /// The audio of the last session started with `keepingAudio`, once it ended well.
    /// Set for one session at a time; `takeAudio()` hands it over.
    private var kept: (audio: CapturedAudio, startedAtUtcMs: Int64)?

    var isBusy: Bool { phase != .idle }

    /* Where the trim rule lives: the screen's save action and the state of the button
     * that triggers it have to agree on what counts as empty. The store keeps its own
     * guard, as the boundary it is. */
    var insertableText: String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var sessionID: UUID?
    private var preparation: Task<Void, Never>?
    private var engine: AVAudioEngine?
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var recognizer: SFSpeechRecognizer?
    private var recognition: SFSpeechRecognitionTask?
    private var interruptionObserver: NSObjectProtocol?
    private var keepsAudio = false
    private var resampler: PCMResampler?
    private var startedAt: Date?

    init() {
        interruptionObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(), queue: .main
        ) { [weak self] notification in
            let kind = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            guard kind == AVAudioSession.InterruptionType.began.rawValue else { return }
            MainActor.assumeIsolated {
                guard let self, self.isBusy else { return }
                /* A voice thought ends where it stands: what was heard is the thought.
                 * Plain dictation has an editor to check, so it is told instead. */
                if self.keepsAudio {
                    self.end(error: nil)
                } else {
                    self.end(error: NSLocalizedString("dictation.interrupted", comment: ""))
                }
            }
        }
    }

    deinit {
        if let interruptionObserver {
            NotificationCenter.default.removeObserver(interruptionObserver)
        }
    }

    /// `keepingAudio` also writes what the microphone hears in the recording capture
    /// format, so a voice thought keeps its sound beside its transcript.
    func start(keepingAudio: Bool = false) {
        guard !isBusy else { return }
        let id = UUID()
        sessionID = id
        phase = .authorizing
        notice = nil
        text = ""
        keepsAudio = keepingAudio
        kept = nil
        preparation = Task { [weak self] in
            guard let self else { return }
            let microphoneGranted = PhoneRecorder.hasPermission
                ? true : await PhoneRecorder.requestPermission()
            guard !Task.isCancelled, sessionID == id else { return }
            guard microphoneGranted else {
                end(error: NSLocalizedString("status.microphoneOff", comment: ""))
                return
            }
            let speechGranted = await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { status in
                    continuation.resume(returning: status == .authorized)
                }
            }
            guard !Task.isCancelled, sessionID == id else { return }
            guard speechGranted else {
                end(error: NSLocalizedString("dictation.speechOff", comment: ""))
                return
            }
            guard let recognizer = SFSpeechRecognizer(locale: .current),
                  recognizer.isAvailable, recognizer.supportsOnDeviceRecognition
            else {
                end(error: NSLocalizedString("dictation.onDeviceUnavailable", comment: ""))
                return
            }
            do {
                try capture(recognizer: recognizer, id: id)
            } catch {
                end(error: error.localizedDescription)
            }
            preparation = nil
        }
    }

    func finish() {
        guard phase == .listening else { return }
        phase = .finishing
        stopAudio()
    }

    /// The kept audio of the session that just ended, once.
    func takeAudio() -> (audio: CapturedAudio, startedAtUtcMs: Int64)? {
        defer { kept = nil }
        return kept
    }

    /// Called when the operator cancels, when a recording takes the microphone away, and
    /// when the dictation screen goes away underneath a running session.
    func cancel() {
        /* A cancelled session has no thought to keep; the file is dropped with it. */
        keepsAudio = false
        end(error: nil)
    }

    private func capture(recognizer: SFSpeechRecognizer, id: UUID) throws {
        let audio = AVAudioSession.sharedInstance()
        try audio.setCategory(.record, mode: .measurement)
        try audio.setActive(true)
        let engine = AVAudioEngine()
        let input = engine.inputNode
        /* Read after the session is active, through the same accessor the recorder uses:
         * an inactive session can report a format the tap will not deliver. */
        let format = input.inputFormat(forBus: 0)
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        if keepsAudio {
            let url = FileManager.default.temporaryDirectory
                .appending(path: "thought-\(id.uuidString).pcm")
            resampler = try PCMResampler(url: url, inputFormat: format)
        }
        let resampler = self.resampler
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            request.append(buffer)
            try? resampler?.append(buffer)
        }
        self.engine = engine
        self.request = request
        self.recognizer = recognizer
        recognition = recognizer.recognitionTask(with: request) { [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal ?? false
            let failure = error?.localizedDescription
            Task { @MainActor in
                self?.receive(text: text, final: final, error: failure, id: id)
            }
        }
        engine.prepare()
        try engine.start()
        startedAt = Date()
        phase = .listening
    }

    private func receive(text: String?, final: Bool, error: String?, id: UUID) {
        guard sessionID == id else { return }
        if let text { self.text = text }
        if final {
            /* With audio kept, silence is still a thought: the sound is what is saved
             * and the sorter files it unread. Without it, nothing was captured. */
            let empty = self.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            end(error: empty && !keepsAudio ? NSLocalizedString("dictation.empty", comment: "") : nil)
        } else if let error {
            end(error: error)
        }
    }

    private func stopAudio() {
        if let engine {
            engine.stop()
            engine.inputNode.removeTap(onBus: 0)
            self.engine = nil
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
        request?.endAudio()
        request = nil
    }

    private func end(error: String?) {
        sessionID = nil
        preparation?.cancel()
        preparation = nil
        stopAudio()
        recognition?.cancel()
        recognition = nil
        recognizer = nil
        if let resampler, let startedAt {
            /* The tap is gone, so the file is complete. Anything heard is kept; a cancel
             * cleared `keepsAudio` and drops it. */
            let finished = try? resampler.finish()
            if keepsAudio, let finished, finished.byteLength > 0 {
                kept = (finished, Int64(startedAt.timeIntervalSince1970 * 1000))
            } else if let finished {
                try? FileManager.default.removeItem(at: finished.url)
            }
        }
        resampler = nil
        startedAt = nil
        keepsAudio = false
        notice = error
        phase = .idle
        onEnded?()
    }
}

import AVFoundation
import Combine
import Foundation
import FoundationModels
import Speech

struct DictationCapture {
    let audio: CapturedAudio?
    let startedAt: Date
    let text: String
    let isThought: Bool
    let error: String?
}

/// The tap and its owner serialize access to the changing recognition request and file.
private final class DictationAudioSink {
    private let lock = NSLock()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var writer: PCMResampler?
    private var writeError: Error?

    func begin(request: SFSpeechAudioBufferRecognitionRequest, writer: PCMResampler) {
        lock.lock()
        defer { lock.unlock() }
        self.request = request
        self.writer = writer
        writeError = nil
    }

    func append(_ buffer: AVAudioPCMBuffer) {
        lock.lock()
        defer { lock.unlock() }
        request?.append(buffer)
        do { try writer?.append(buffer) } catch { writeError = error }
    }

    func finish() throws -> CapturedAudio? {
        lock.lock()
        defer { lock.unlock() }
        request?.endAudio()
        request = nil
        let finishedWriter = writer
        writer = nil
        let audio = try finishedWriter?.finish()
        if let writeError {
            if let audio { try? FileManager.default.removeItem(at: audio.url) }
            throw writeError
        }
        return audio
    }
}

/// One microphone owner. A warm session runs the tap but discards buffers between dictations.
@MainActor
final class PhoneDictation: ObservableObject {
    enum Phase { case idle, authorizing, listening, finishing }
    @Published private(set) var phase: Phase = .idle
    @Published var text = ""
    @Published private(set) var notice: String?
    @Published private(set) var warmUntil: Date?
    var onEnded: ((DictationCapture) -> Void)?
    var onStateChanged: (() -> Void)?
    @Published var profile = DictationProfile.empty

    var isBusy: Bool { phase != .idle }
    var insertableText: String { text.trimmingCharacters(in: .whitespacesAndNewlines) }
    var isWarm: Bool { engine?.isRunning == true && (warmUntil ?? .distantPast) > Date() }
    static var stylesAvailable: Bool { SystemLanguageModel.default.isAvailable }

    private let sink = DictationAudioSink()
    private var sessionID: UUID?
    private var preparation: Task<Void, Never>?
    private var deadline: Task<Void, Never>?
    private var finishDeadline: Task<Void, Never>?
    private var engine: AVAudioEngine?
    private var recognizer: SFSpeechRecognizer?
    private var recognition: SFSpeechRecognitionTask?
    private var observers: [NSObjectProtocol] = []
    private var startedAt = Date()
    private var isThought = false
    private var audio: CapturedAudio?
    private var completion: Task<Void, Never>?
    private var sessionProfile = DictationProfile.empty
    private var sessionStyle: DictationProfile.Style?

    init() {
        observers.append(NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(), queue: .main
        ) { [weak self] notification in
            let kind = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
            guard kind == AVAudioSession.InterruptionType.began.rawValue else { return }
            MainActor.assumeIsolated { self?.interrupt() }
        })
        observers.append(NotificationCenter.default.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.interrupt() }
        })
    }

    deinit { for observer in observers { NotificationCenter.default.removeObserver(observer) } }

    func start(keepingAudio: Bool = false, keyboard: Bool = false) {
        guard !isBusy else { return }
        let id = UUID()
        sessionID = id
        phase = .authorizing
        notice = nil
        text = ""
        isThought = keepingAudio
        audio = nil
        startedAt = Date()
        sessionProfile = profile
        let styleID = UserDefaults.standard.string(forKey: DictationPreferences.styleKey)
        sessionStyle = profile.styles.first { $0.id == styleID }
        onStateChanged?()
        preparation = Task { [weak self] in
            guard let self else { return }
            let microphoneGranted = PhoneRecorder.hasPermission ? true : await PhoneRecorder.requestPermission()
            guard !Task.isCancelled, sessionID == id else { return }
            guard microphoneGranted else {
                settle(error: NSLocalizedString("status.microphoneOff", comment: ""))
                return
            }
            let speechGranted = await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { status in
                    continuation.resume(returning: status == .authorized)
                }
            }
            guard !Task.isCancelled, sessionID == id else { return }
            guard speechGranted else {
                settle(error: NSLocalizedString("dictation.speechOff", comment: ""))
                return
            }
            guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: DictationPreferences.activeLanguage)),
                  recognizer.isAvailable, recognizer.supportsOnDeviceRecognition
            else {
                settle(error: NSLocalizedString("dictation.onDeviceUnavailable", comment: ""))
                return
            }
            do {
                try capture(recognizer: recognizer, id: id)
                if keyboard { extendWarmSession() }
            } catch { settle(error: error.localizedDescription) }
            preparation = nil
        }
    }

    func finish() {
        guard phase == .listening else { return }
        phase = .finishing
        closeCapture()
        onStateChanged?()
        // A speech service that never returns a final result must not strand the keyboard.
        finishDeadline = Task { [weak self] in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled, let self, phase == .finishing, completion == nil else { return }
            completeRecognition(error: nil)
        }
    }

    func cancel() {
        guard isBusy else { return }
        closeCapture()
        if let audio { try? FileManager.default.removeItem(at: audio.url) }
        audio = nil
        stopRecognition()
        phase = .idle
        notice = nil
        if !isWarm { stopEngine() }
        onStateChanged?()
    }

    func endWarmSession() {
        warmUntil = nil
        deadline?.cancel()
        deadline = nil
        switch phase {
        case .authorizing: cancel()
        case .listening: finish()
        case .finishing, .idle: stopEngine()
        }
        onStateChanged?()
    }

    private func extendWarmSession() {
        warmUntil = Date().addingTimeInterval(TimeInterval(DictationPreferences.warmMinutes * 60))
        deadline?.cancel()
        deadline = Task { [weak self] in
            try? await Task.sleep(for: .seconds(DictationPreferences.warmMinutes * 60))
            guard !Task.isCancelled, let self else { return }
            warmUntil = nil
            if phase == .listening { finish() } else if phase == .idle { stopEngine() }
            onStateChanged?()
        }
        onStateChanged?()
    }

    private func capture(recognizer: SFSpeechRecognizer, id: UUID) throws {
        if engine?.isRunning != true {
            stopEngine()
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playAndRecord, mode: .measurement, options: [.mixWithOthers, .defaultToSpeaker])
            try session.setActive(true)
            let engine = AVAudioEngine()
            let input = engine.inputNode
            let format = input.inputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else { throw CaptureError.unsupportedFormat }
            let sink = self.sink
            input.installTap(onBus: 0, bufferSize: 4096, format: format) { buffer, _ in sink.append(buffer) }
            self.engine = engine
            engine.prepare()
            try engine.start()
        }
        guard let engine else { throw CaptureError.unsupportedFormat }
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        request.contextualStrings = sessionProfile.recognitionHints
        let file = FileManager.default.temporaryDirectory.appending(path: "dictation-\(id.uuidString).pcm")
        let writer = try PCMResampler(url: file, inputFormat: engine.inputNode.inputFormat(forBus: 0))
        self.recognizer = recognizer
        recognition = recognizer.recognitionTask(with: request) { [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal ?? false
            let failure = error?.localizedDescription
            Task { @MainActor in self?.receive(text: text, final: final, error: failure, id: id) }
        }
        sink.begin(request: request, writer: writer)
        startedAt = Date()
        phase = .listening
        onStateChanged?()
    }

    private func receive(text: String?, final: Bool, error: String?, id: UUID) {
        guard sessionID == id, completion == nil else { return }
        if let text { self.text = text }
        if final || error != nil { completeRecognition(error: final ? nil : error) }
    }

    private func completeRecognition(error: String?) {
        guard isBusy, completion == nil else { return }
        closeCapture()
        phase = .finishing
        finishDeadline?.cancel()
        finishDeadline = nil
        let corrected = sessionProfile.apply(to: insertableText)
        text = corrected
        onStateChanged?()
        guard error == nil, !corrected.isEmpty, let style = sessionStyle else {
            settle(error: error)
            return
        }
        guard Self.stylesAvailable else {
            settle(error: nil, styleNotice: NSLocalizedString("dictation.styleUnavailable", comment: ""))
            return
        }
        completion = Task { [weak self] in
            guard let self else { return }
            do {
                struct Envelope: Encodable {
                    let transcript: String
                    let language: String
                    let target: [String: String] = [:]
                    let context: [String: String] = [:]
                }
                let envelope = try JSONEncoder().encode(Envelope(transcript: corrected, language: DictationPreferences.activeLanguage))
                let session = LanguageModelSession(instructions: style.prompt + "\nReturn only the rewritten text. An empty response is valid when there are no words to keep.")
                let response = try await session.respond(to: String(decoding: envelope, as: UTF8.self))
                guard !Task.isCancelled else { return }
                text = response.content.trimmingCharacters(in: .whitespacesAndNewlines)
                settle(error: nil)
            } catch {
                guard !Task.isCancelled else { return }
                settle(error: nil, styleNotice: NSLocalizedString("dictation.styleFailed", comment: ""))
            }
        }
    }

    private func interrupt() {
        warmUntil = nil
        deadline?.cancel()
        if isBusy {
            closeCapture()
            settle(error: NSLocalizedString("dictation.interrupted", comment: ""))
        } else { stopEngine(); onStateChanged?() }
    }

    private func closeCapture() {
        do {
            if let captured = try sink.finish() { audio = captured }
        } catch { notice = NSLocalizedString("status.notSaved", comment: "") }
        if !isWarm { stopEngine() }
    }

    private func settle(error: String?, styleNotice: String? = nil) {
        guard isBusy else { return }
        closeCapture()
        let capture = DictationCapture(audio: audio, startedAt: startedAt, text: insertableText,
                                       isThought: isThought, error: error)
        audio = nil
        stopRecognition()
        phase = .idle
        notice = error ?? styleNotice ?? (insertableText.isEmpty ? NSLocalizedString("dictation.empty", comment: "") : notice)
        if error != nil { warmUntil = nil; stopEngine() }
        onEnded?(capture)
        onStateChanged?()
    }

    private func stopRecognition() {
        sessionID = nil
        preparation?.cancel()
        preparation = nil
        finishDeadline?.cancel()
        finishDeadline = nil
        completion?.cancel()
        completion = nil
        recognition?.cancel()
        recognition = nil
        recognizer = nil
    }

    private func stopEngine() {
        if let engine {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            self.engine = nil
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
    }
}

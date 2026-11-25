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

    func append(_ buffer: AVAudioPCMBuffer) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        request?.append(buffer)
        do { try writer?.append(buffer) } catch { writeError = error }
        return request != nil
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
    @Published private(set) var phase: Phase = .idle {
        didSet { if phase != .listening { meter.reset() } }
    }
    @Published var text = ""
    @Published private(set) var notice: String?
    @Published private(set) var warmUntil: Date?
    var onEnded: ((DictationCapture) -> Void)?
    var onStateChanged: (() -> Void)?
    @Published var profile = DictationProfile.empty
    let meter = InputLevelMeter()

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
    private let rewrite = BoundedRewrite()
    private var sessionProfile = DictationProfile.empty
    private var sessionStyle: DictationProfile.Style?
    private var warmStyleRequestID: UUID?
    private var sessionLanguage = DictationPreferences.activeLanguage
    #if DEBUG
    private var script: Task<Void, Never>?
    #endif

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

    /// `language` is the keyboard's typing language when the keyboard asked for this dictation;
    /// `style` is a style id one recording asked for, empty for none, nil for the chosen style.
    func start(keepingAudio: Bool = false, keyboard: Bool = false, language: String? = nil,
               style: String? = nil, warmStyleRequestID: UUID? = nil) {
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
        sessionLanguage = DictationPreferences.recognitionLanguage(for: language)
        sessionStyle = styleForSession(style ?? DictationPreferences.style)
        self.warmStyleRequestID = warmStyleRequestID
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
            guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: sessionLanguage)),
                  recognizer.isAvailable, recognizer.supportsOnDeviceRecognition
            else {
                settle(error: NSLocalizedString("dictation.onDeviceUnavailable", comment: ""))
                return
            }
            do {
                try capture(recognizer: recognizer, id: id)
                if keyboard { extendWarmSession() }
            } catch { settle(error: NSLocalizedString("dictation.failed", comment: "")) }
            preparation = nil
        }
    }

    func finish() {
        guard phase == .listening else { return }
        phase = .finishing
        closeCapture()
        onStateChanged?()
        #if DEBUG
        if script != nil, let sessionID {
            script?.cancel()
            script = nil
            receive(text: text, final: true, failed: false, id: sessionID)
            return
        }
        #endif
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
            let meter = self.meter
            input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, _ in
                guard sink.append(buffer) else { return }
                let level = InputLevelMeter.level(of: buffer)
                Task { @MainActor in
                    guard self?.phase == .listening else { return }
                    meter.push(level)
                }
            }
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
        #if DEBUG
        if let words = Self.scriptedWords {
            feed(words, id: id)
        } else {
            recognize(request, with: recognizer, id: id)
        }
        #else
        recognize(request, with: recognizer, id: id)
        #endif
        sink.begin(request: request, writer: writer)
        startedAt = Date()
        phase = .listening
        onStateChanged?()
    }

    private func recognize(_ request: SFSpeechAudioBufferRecognitionRequest, with recognizer: SFSpeechRecognizer, id: UUID) {
        recognition = recognizer.recognitionTask(with: request) { [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal ?? false
            let failed = error != nil
            Task { @MainActor in self?.receive(text: text, final: final, failed: failed, id: id) }
        }
    }

    #if DEBUG
    /// Debug builds launched with `-SonaScriptedRecognition "some words"` hear those words
    /// instead of asking the speech service, which the simulator cannot run. The microphone
    /// still runs; the words arrive one at a time as partial results, and finishing makes
    /// what has arrived final.
    private static var scriptedWords: [String]? {
        UserDefaults.standard.string(forKey: "SonaScriptedRecognition").map { $0.split(separator: " ").map(String.init) }
    }

    private func feed(_ words: [String], id: UUID) {
        script = Task { [weak self] in
            for count in words.indices {
                try? await Task.sleep(for: .milliseconds(450))
                guard !Task.isCancelled else { return }
                self?.receive(text: words[...count].joined(separator: " "), final: false, failed: false, id: id)
            }
        }
    }
    #endif

    /// Each partial result goes to the keyboard too, so the words show there as they are heard.
    /// A recognizer that stops while still listening keeps the words it heard and says it
    /// stopped; after Finish, its ending is the dictation finishing; with no words heard it is
    /// the empty case.
    private func receive(text: String?, final: Bool, failed: Bool, id: UUID) {
        guard sessionID == id, completion == nil else { return }
        if let text, text != self.text {
            self.text = text
            if !final { onStateChanged?() }
        }
        if final {
            completeRecognition(error: nil)
        } else if failed {
            let stoppedEarly = phase == .listening && !insertableText.isEmpty
            completeRecognition(error: nil, notice: stoppedEarly ? NSLocalizedString("dictation.stoppedEarly", comment: "") : nil)
        }
    }

    private func completeRecognition(error: String?, notice: String? = nil) {
        guard isBusy, completion == nil else { return }
        closeCapture()
        phase = .finishing
        finishDeadline?.cancel()
        finishDeadline = nil
        let language = sessionLanguage
        let corrected = sessionProfile.apply(to: DictationCleanup.clean(insertableText, language: language))
        text = corrected
        onStateChanged?()
        let style = latestSessionStyle()
        guard error == nil, notice == nil, !corrected.isEmpty, let style else {
            settle(error: error, styleNotice: notice)
            return
        }
        guard Self.stylesAvailable else {
            settle(error: nil, styleNotice: NSLocalizedString("dictation.styleUnavailable", comment: ""))
            return
        }
        let id = sessionID
        let prompt = style.prompt
        completion = Task { [weak self] in
            guard let self else { return }
            let outcome = await rewrite.run {
                struct Envelope: Encodable {
                    let transcript: String
                    let language: String
                    let target: [String: String] = [:]
                    let context: [String: String] = [:]
                }
                let envelope = try JSONEncoder().encode(Envelope(transcript: corrected, language: language))
                let session = LanguageModelSession(instructions: prompt + "\nReturn only the rewritten text.")
                return try await session.respond(to: String(decoding: envelope, as: UTF8.self)).content
            }
            guard !Task.isCancelled, sessionID == id else { return }
            switch outcome {
            case .rewritten(let output):
                text = DictationCleanup.rewritten(output)
                settle(error: nil)
            case .fallback:
                settle(error: nil, styleNotice: NSLocalizedString("dictation.styleFailed", comment: ""))
            case .cancelled:
                break
            }
        }
    }

    private func latestSessionStyle() -> DictationProfile.Style? {
        guard let requestID = warmStyleRequestID,
              let selected = try? KeyboardSessionStore.shared().takeStyle(for: requestID) else { return sessionStyle }
        return styleForSession(selected)
    }

    private func styleForSession(_ id: String) -> DictationProfile.Style? {
        sessionProfile.styles.first { $0.id == id }.map {
            DictationProfile.Style(id: $0.id, name: $0.name, prompt: DictationTone.prompt(of: $0))
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
        #if DEBUG
        script?.cancel()
        script = nil
        #endif
        sessionID = nil
        warmStyleRequestID = nil
        preparation?.cancel()
        preparation = nil
        finishDeadline?.cancel()
        finishDeadline = nil
        rewrite.cancel()
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

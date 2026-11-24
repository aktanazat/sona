import AVFoundation
import OSLog
import Speech
import UIKit
import WebKit

private let log = Logger(subsystem: "com.aktanazat.sona.mobile.keyboard", category: "microphone")

/// A page that opens the microphone when asked and posts what it hears.
@MainActor
protocol CapturePage: AnyObject {
    /// Each message the page posts: `ready`, `started`, `audio` with base64 samples,
    /// `stopped`, `failed` or `interrupted` with a reason.
    var onMessage: (([String: Any]) -> Void)? { get set }
    /// The page could not run a call.
    var onFailure: ((String) -> Void)? { get set }
    /// The page's process ended and the page is loading again; it posts `ready` once loaded.
    var onReload: (() -> Void)? { get set }
    func attach(to view: UIView)
    func run(_ call: String)
}

/// Starts recognizing what a request is fed, in a language, reporting text, whether it is
/// final, and a failure. Returns what cancels it, or nil when no recognizer for the
/// language runs on this iPhone.
typealias Recognize = @MainActor (
    SFSpeechAudioBufferRecognitionRequest, String, @escaping @Sendable (String?, Bool, String?) -> Void
) -> (() -> Void)?

/// The keyboard's own microphone and recognizer.
///
/// iOS starts no audio input in a keyboard process, even with Full Access. WebKit captures
/// in its own processes under the containing app's microphone permission, so a page in an
/// almost invisible web view opens the microphone and posts 16 kHz samples here, and Apple's
/// recognizer, which also runs outside this process, turns them into words on this iPhone.
@MainActor
final class KeyboardMicrophone {
    static let sampleRate = 16_000.0

    struct Timing {
        /// How long the page has to load, the first time Dictate is touched.
        var load: Duration = .seconds(10)
        /// How long the page has to say the microphone opened once asked, before it
        /// counts as refused.
        var start: Duration = .seconds(3)
        /// How long a stop waits for the page's last samples.
        var drain: Duration = .milliseconds(150)
        /// How long the recognizer has to finish after the audio ends.
        var finish: Duration = .seconds(3)
    }

    var onEvent: ((KeyboardDictation.Event) -> Void)?
    var onNotice: ((String) -> Void)?

    private let page: CapturePage
    private let recognize: Recognize
    private let timing: Timing
    private let queue: KeyboardDictationQueue?
    private let format = AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 1)!
    private var ready = false
    private var wanted = false
    private var started = false
    private var draining = false
    private var run = 0
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var cancelRecognition: (() -> Void)?
    private var heartbeat: Timer?
    private var deadline: Task<Void, Never>?
    private var samples = 0
    private var audio: KeyboardAudioFile?
    #if DEBUG
    private var script: [String]?
    #endif

    init(
        page: CapturePage? = nil, recognize: @escaping Recognize = KeyboardMicrophone.apple,
        timing: Timing = Timing(), queue: KeyboardDictationQueue? = try? KeyboardDictationQueue.shared()
    ) {
        self.page = page ?? WebCapturePage()
        self.recognize = recognize
        self.timing = timing
        self.queue = queue
        self.page.onMessage = { [weak self] message in self?.receive(message) }
        self.page.onFailure = { [weak self] reason in self?.refused(reason) }
        self.page.onReload = { [weak self] in
            self?.ready = false
            self?.refused("web content process ended")
        }
    }

    /// WebKit throttles a web view outside a window, so the page stays in the keyboard's view.
    func attach(to view: UIView) {
        page.attach(to: view)
    }

    func start(language: String, hints: [String]) {
        abandon()
        discardAudio()
        run += 1
        let run = run
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        request.contextualStrings = Array(hints.prefix(100))
        samples = 0
        let report: @Sendable (String?, Bool, String?) -> Void = { [weak self] text, final, failure in
            Task { @MainActor in self?.recognized(text: text, final: final, failure: failure, run: run) }
        }
        #if DEBUG
        script = Self.scriptedWords
        if script == nil { cancelRecognition = recognize(request, language, report) }
        #else
        cancelRecognition = recognize(request, language, report)
        #endif
        #if DEBUG
        let unavailable = script == nil && cancelRecognition == nil
        #else
        let unavailable = cancelRecognition == nil
        #endif
        if unavailable {
            log.error("no on-device recognizer for \(language, privacy: .public)")
            onNotice?(NSLocalizedString("dictation.onDeviceUnavailable", comment: ""))
            onEvent?(.recognizerUnavailable)
            return
        }
        self.request = request
        audio = queue.flatMap { try? KeyboardAudioFile(in: $0) }
        wanted = true
        heartbeat = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.started else { return }
                self.page.run("captureHeartbeat()")
            }
        }
        if ready { beginCapture() } else { refuse(after: timing.load) }
    }

    /// The start time runs from when the page is asked, so a page still loading on the
    /// first Dictate does not count against the microphone.
    private func beginCapture() {
        page.run("startCapture()")
        refuse(after: timing.start)
    }

    private func refuse(after delay: Duration) {
        let run = run
        deadline?.cancel()
        deadline = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled, let self, self.run == run, wanted, !started else { return }
            log.error("capture did not start in time")
            onEvent?(.captureRefused)
        }
    }

    /// Asks the page for the samples it still holds, so the last syllable is heard, then
    /// ends the audio; the recognizer finishes the words it has, or gives up after a while
    /// with what it heard.
    func end() {
        guard request != nil, !draining else { return }
        guard started else { endAudio(); return }
        draining = true
        page.run("finishCapture()")
        let run = run
        deadline?.cancel()
        deadline = Task { [weak self, timing] in
            try? await Task.sleep(for: timing.drain)
            guard !Task.isCancelled, let self, self.run == run, draining else { return }
            endAudio()
        }
    }

    func abandon() {
        stopPage()
        deadline?.cancel()
        deadline = nil
        cancelRecognition?()
        cancelRecognition = nil
        request = nil
        #if DEBUG
        script = nil
        #endif
    }

    /// The finished dictation's audio, written to the queue's folder, for the record of it.
    func takeAudio() -> KeyboardDictationRecord.Audio? {
        defer { audio = nil }
        return try? audio?.finish()
    }

    private func discardAudio() {
        guard let audio, let queue else { return }
        _ = try? audio.finish()
        try? FileManager.default.removeItem(at: queue.audioURL(audio.name))
        self.audio = nil
    }

    private func endAudio() {
        draining = false
        stopPage()
        request?.endAudio()
        #if DEBUG
        if let script {
            let text = scriptedText(script)
            abandon()
            onEvent?(.heard(text, final: true))
            return
        }
        #endif
        let run = run
        deadline?.cancel()
        deadline = Task { [weak self, timing] in
            try? await Task.sleep(for: timing.finish)
            guard !Task.isCancelled, let self, self.run == run, request != nil else { return }
            onEvent?(.finishTimedOut)
        }
    }

    private func stopPage() {
        heartbeat?.invalidate()
        heartbeat = nil
        draining = false
        if wanted {
            page.run("stopCapture()")
            log.info("captured \(self.samples) samples")
        }
        wanted = false
        started = false
    }

    private func refused(_ reason: String) {
        guard wanted else { return }
        log.error("capture refused: \(reason, privacy: .public)")
        onEvent?(started ? .captureEnded : .captureRefused)
    }

    private func recognized(text: String?, final: Bool, failure: String?, run: Int) {
        guard run == self.run, request != nil else { return }
        if final {
            abandon()
            if let text { onEvent?(.heard(text, final: true)) }
            return
        }
        if let text { onEvent?(.heard(text, final: false)) }
        if text == nil, let failure {
            log.error("recognition failed: \(failure, privacy: .public)")
            abandon()
            onEvent?(.recognitionFailed)
        }
    }

    private func receive(_ message: [String: Any]) {
        switch message["type"] as? String {
        case "ready":
            ready = true
            if wanted { beginCapture() }
        case "started":
            guard wanted else { return }
            started = true
            log.info("capture started at \(message["sampleRate"] as? Double ?? 0) Hz")
            onEvent?(.captureStarted)
        case "audio":
            guard wanted, started, let base64 = message["data"] as? String,
                  let data = Data(base64Encoded: base64) else { return }
            append(data)
        case "stopped":
            if draining { endAudio() }
        case let type? where type == "failed" || type == "interrupted":
            refused("\(type): \(message["reason"] as? String ?? "")")
        default:
            break
        }
    }

    private func append(_ data: Data) {
        let frames = data.count / MemoryLayout<Int16>.size
        guard frames > 0, let request,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(frames)),
              let channel = buffer.floatChannelData?[0]
        else { return }
        buffer.frameLength = AVAudioFrameCount(frames)
        data.withUnsafeBytes { raw in
            for index in 0..<frames {
                channel[index] = Float(Int16(littleEndian: raw.loadUnaligned(fromByteOffset: index * 2, as: Int16.self))) / 32_768
            }
        }
        samples += frames
        request.append(buffer)
        try? audio?.append(data)
        #if DEBUG
        if let script, samples / 8_000 != (samples - frames) / 8_000 {
            onEvent?(.heard(scriptedText(script), final: false))
        }
        #endif
    }

    /// Apple's on-device recognizer, which runs in its own process.
    static func apple(
        _ request: SFSpeechAudioBufferRecognitionRequest, language: String,
        report: @escaping @Sendable (String?, Bool, String?) -> Void
    ) -> (() -> Void)? {
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: language)),
              recognizer.isAvailable, recognizer.supportsOnDeviceRecognition
        else { return nil }
        let task = recognizer.recognitionTask(with: request) { result, error in
            report(result?.bestTranscription.formattedString, result?.isFinal ?? false, error.map { "\($0)" })
        }
        return { task.cancel() }
    }

    #if DEBUG
    /* Debug builds with `SonaScriptedRecognition` set in the shared container hear those
     * words instead of asking the speech service, which the simulator cannot run: one more
     * word for each half second of audio the page delivers. */
    private static var scriptedWords: [String]? {
        DictationPreferences.shared.string(forKey: "SonaScriptedRecognition").map { $0.split(separator: " ").map(String.init) }
    }

    private func scriptedText(_ words: [String]) -> String {
        words.prefix(samples / 8_000).joined(separator: " ")
    }
    #endif
}

/// The capture page in a 2-point web view that is all but invisible.
@MainActor
final class WebCapturePage: NSObject, CapturePage, WKUIDelegate, WKNavigationDelegate {
    private static let origin = URL(string: "https://keyboard.sona.invalid/")!
    var onMessage: (([String: Any]) -> Void)?
    var onFailure: ((String) -> Void)?
    var onReload: (() -> Void)?
    private let webView: WKWebView

    override init() {
        let configuration = WKWebViewConfiguration()
        configuration.allowsInlineMediaPlayback = true
        configuration.mediaTypesRequiringUserActionForPlayback = []
        configuration.websiteDataStore = .nonPersistent()
        webView = WKWebView(frame: CGRect(x: 0, y: 0, width: 2, height: 2), configuration: configuration)
        super.init()
        configuration.userContentController.add(WeakHandler(self), name: "capture")
        webView.uiDelegate = self
        webView.navigationDelegate = self
        webView.alpha = 0.02
        webView.isOpaque = false
        webView.isUserInteractionEnabled = false
        webView.accessibilityElementsHidden = true
    }

    func attach(to view: UIView) {
        guard webView.superview == nil else { return }
        view.insertSubview(webView, at: 0)
        webView.loadHTMLString(Self.page, baseURL: Self.origin)
    }

    func run(_ call: String) {
        webView.evaluateJavaScript(call + "; 0") { [weak self] _, error in
            guard let error else { return }
            MainActor.assumeIsolated { self?.onFailure?("\(call): \(error.localizedDescription)") }
        }
    }

    fileprivate func receive(_ body: Any) {
        if let message = body as? [String: Any] { onMessage?(message) }
    }

    /// Skips WebKit's own per-page prompt; the app's microphone permission still decides.
    func webView(
        _ webView: WKWebView,
        requestMediaCapturePermissionFor origin: WKSecurityOrigin,
        initiatedByFrame frame: WKFrameInfo,
        type: WKMediaCaptureType,
        decisionHandler: @escaping @MainActor (WKPermissionDecision) -> Void
    ) {
        decisionHandler(type == .microphone ? .grant : .deny)
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        onReload?()
        webView.loadHTMLString(Self.page, baseURL: Self.origin)
    }

    /* The page opens the microphone only when asked, since an AudioContext made earlier
     * can take audio focus or move a Bluetooth route while the keyboard is merely shown.
     * It stops itself when native code stops calling captureHeartbeat, so a keyboard that
     * was suspended or crashed cannot leave the microphone on. finishCapture posts the
     * samples still in the processor's buffer before it stops. */
    private static let page = """
    <!doctype html><html><body><script>
    const targetRate = \(Int(KeyboardMicrophone.sampleRate));
    const post = message => window.webkit.messageHandlers.capture.postMessage(message);
    const encode = samples => {
      const bytes = new Uint8Array(samples.buffer, 0, samples.length * 2);
      let binary = '';
      for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      return btoa(binary);
    };
    let lastHeartbeat = performance.now();
    window.captureHeartbeat = () => { lastHeartbeat = performance.now(); };
    let stopCapture = () => {};
    let finishCapture = () => {};
    window.stopCapture = () => stopCapture();
    window.finishCapture = () => finishCapture();
    window.startCapture = async () => {
      stopCapture();
      let stopping = false, draining = false, stream, context, source, processor, sink, watchdog;
      const audioSession = navigator.audioSession;
      const previousType = audioSession?.type ?? 'auto';
      const cleanup = () => {
        if (stopping) return;
        stopping = true;
        clearInterval(watchdog);
        if (processor) { processor.onaudioprocess = null; processor.disconnect(); }
        source?.disconnect();
        sink?.disconnect();
        stream?.getTracks().forEach(track => track.stop());
        context?.close().catch(() => {});
        try { if (audioSession) audioSession.type = previousType; } catch (_) {}
      };
      const interrupted = reason => { if (!stopping) { cleanup(); post({ type: 'interrupted', reason }); } };
      stopCapture = cleanup;
      finishCapture = () => { draining = true; };
      try {
        try { if (audioSession) audioSession.type = 'play-and-record'; } catch (_) {}
        stream = await navigator.mediaDevices.getUserMedia({
          audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
        });
        if (stopping) { stream.getTracks().forEach(track => track.stop()); return; }
        stream.getAudioTracks()[0].onended = () => interrupted('track-ended');
        context = new AudioContext({ latencyHint: 'interactive' });
        await context.resume();
        if (stopping) { cleanup(); return; }
        processor = context.createScriptProcessor(1024, 1, 1);
        sink = context.createGain();
        sink.gain.value = 0;
        processor.connect(sink);
        sink.connect(context.destination);
        source = context.createMediaStreamSource(stream);
        source.connect(processor);
        context.onstatechange = () => { if (context.state !== 'running') interrupted('context-' + context.state); };
        if (audioSession) audioSession.onstatechange = () => { if (audioSession.state === 'interrupted') interrupted('audio-session-interrupted'); };
        lastHeartbeat = performance.now();
        watchdog = setInterval(() => { if (performance.now() - lastHeartbeat > 3000) interrupted('heartbeat-expired'); }, 500);
        const step = context.sampleRate / targetRate;
        let position = 0, sum = 0, count = 0;
        processor.onaudioprocess = event => {
          const input = event.inputBuffer.getChannelData(0);
          const output = new Int16Array(Math.ceil(input.length / step) + 1);
          let written = 0;
          for (const sample of input) {
            sum += sample; count += 1; position += 1;
            if (position >= step) {
              position -= step;
              output[written++] = Math.max(-1, Math.min(1, sum / count)) * 32767;
              sum = 0; count = 0;
            }
          }
          if (written > 0) post({ type: 'audio', data: encode(output.subarray(0, written)) });
          if (draining) { cleanup(); post({ type: 'stopped' }); }
        };
        post({ type: 'started', sampleRate: context.sampleRate });
      } catch (error) {
        if (stopping) return;
        cleanup();
        post({ type: 'failed', reason: error.name + ': ' + error.message });
      }
    };
    window.addEventListener('pagehide', () => stopCapture());
    post({ type: 'ready' });
    </script></body></html>
    """
}

/// The content controller keeps its handler alive; this keeps it from keeping the page.
private final class WeakHandler: NSObject, WKScriptMessageHandler {
    weak var target: WebCapturePage?
    init(_ target: WebCapturePage) { self.target = target }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        MainActor.assumeIsolated { target?.receive(message.body) }
    }
}

#if DEBUG
/// Debug builds log how soon the keyboard showed it was listening after Dictate, how soon
/// the microphone opened, and how many frames the keyboard dropped while dictating or
/// while it appeared and loaded the page.
@MainActor
final class FrameMeter: NSObject {
    private var link: CADisplayLink?
    private var tapped: CFTimeInterval = 0
    private var last: CFTimeInterval = 0
    private var frames = 0
    private var dropped = 0
    private var label = "Dictate"

    func begin(label: String = "Dictate") {
        end()
        self.label = label
        tapped = CACurrentMediaTime()
        last = 0
        frames = 0
        dropped = 0
        let link = CADisplayLink(target: self, selector: #selector(tick))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    func mark(_ moment: String) {
        guard link != nil else { return }
        log.info("\(moment, privacy: .public) \(Int((CACurrentMediaTime() - self.tapped) * 1000)) ms after \(self.label, privacy: .public)")
    }

    func end() {
        guard let link else { return }
        link.invalidate()
        self.link = nil
        log.info("\(self.label, privacy: .public): drew \(self.frames) frames and dropped \(self.dropped)")
    }

    @objc private func tick(_ link: CADisplayLink) {
        if frames == 0 {
            log.info("first frame \(Int((link.targetTimestamp - self.tapped) * 1000)) ms after \(self.label, privacy: .public)")
        }
        let interval = link.targetTimestamp - link.timestamp
        if last > 0, interval > 0 {
            dropped += max(0, Int(((link.timestamp - last) / interval).rounded()) - 1)
        }
        last = link.timestamp
        frames += 1
    }
}
#endif

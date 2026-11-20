import AVFoundation
import OSLog
import Speech
import UIKit
import WebKit

private let log = Logger(subsystem: "com.aktanazat.sona.mobile.keyboard", category: "microphone")

/// The keyboard's own microphone and recognizer.
///
/// iOS starts no audio input in a keyboard process, even with Full Access. WebKit captures
/// in its own processes under the containing app's microphone permission, so a page in an
/// almost invisible web view opens the microphone and posts 16 kHz samples here, and Apple's
/// recognizer, which also runs outside this process, turns them into words on this iPhone.
@MainActor
final class KeyboardMicrophone: NSObject {
    static let sampleRate = 16_000.0
    private static let origin = URL(string: "https://keyboard.sona.invalid/")!

    var onEvent: ((KeyboardDictation.Event) -> Void)?
    var onNotice: ((String) -> Void)?

    private let webView: WKWebView
    private let format = AVAudioFormat(standardFormatWithSampleRate: sampleRate, channels: 1)!
    private var ready = false
    private var wanted = false
    private var started = false
    private var run = 0
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var recognition: SFSpeechRecognitionTask?
    private var heartbeat: Timer?
    private var deadline: Task<Void, Never>?
    private var samples = 0
    #if DEBUG
    private var script: [String]?
    private var captured = Data()
    #endif

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

    /// WebKit throttles a web view outside a window, so it stays in the keyboard's view,
    /// loaded before the first tap so Dictate starts at once.
    func attach(to view: UIView) {
        guard webView.superview == nil else { return }
        view.insertSubview(webView, at: 0)
        webView.loadHTMLString(Self.page, baseURL: Self.origin)
    }

    func start(language: String, hints: [String]) {
        abandon()
        run += 1
        let run = run
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = true
        request.addsPunctuation = true
        request.taskHint = .dictation
        request.contextualStrings = Array(hints.prefix(100))
        samples = 0
        #if DEBUG
        captured = Data()
        script = Self.scriptedWords
        if script == nil, !recognize(request, language: language, run: run) { return }
        #else
        guard recognize(request, language: language, run: run) else { return }
        #endif
        self.request = request
        wanted = true
        heartbeat = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { _ = self?.webView.evaluateJavaScript("captureHeartbeat(); 0") }
        }
        if ready { beginPage() }
        deadline = Task { [weak self] in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled, let self, self.run == run, wanted, !started else { return }
            log.error("capture did not start in time")
            onEvent?(.captureRefused)
        }
    }

    private func recognize(_ request: SFSpeechAudioBufferRecognitionRequest, language: String, run: Int) -> Bool {
        guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: language)),
              recognizer.isAvailable, recognizer.supportsOnDeviceRecognition
        else {
            onNotice?(NSLocalizedString("dictation.onDeviceUnavailable", comment: ""))
            onEvent?(.recognitionFailed)
            return false
        }
        recognition = recognizer.recognitionTask(with: request) { [weak self] result, error in
            let text = result?.bestTranscription.formattedString
            let final = result?.isFinal ?? false
            let failure = error.map { "\($0)" }
            Task { @MainActor in self?.recognized(text: text, final: final, failure: failure, run: run) }
        }
        return true
    }

    /// Stops the microphone; the recognizer finishes the words it has, or gives up after
    /// three seconds with what it heard.
    func end() {
        guard request != nil else { return }
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
        deadline = Task { [weak self] in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled, let self, self.run == run, request != nil else { return }
            onEvent?(.finishTimedOut)
        }
    }

    func abandon() {
        stopPage()
        deadline?.cancel()
        deadline = nil
        recognition?.cancel()
        recognition = nil
        request = nil
        #if DEBUG
        script = nil
        #endif
    }

    private func beginPage() {
        webView.evaluateJavaScript("startCapture(); 0") { [weak self] _, error in
            guard let error else { return }
            MainActor.assumeIsolated { self?.refused("start: \(error.localizedDescription)") }
        }
    }

    private func stopPage() {
        heartbeat?.invalidate()
        heartbeat = nil
        if wanted {
            webView.evaluateJavaScript("stopCapture(); 0")
            log.info("captured \(self.samples) samples")
            #if DEBUG
            keepCapture()
            #endif
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
        if let text { onEvent?(.heard(text, final: final)) }
        if final {
            abandon()
        } else if text == nil, let failure {
            log.error("recognition failed: \(failure, privacy: .public)")
            abandon()
            onNotice?(NSLocalizedString("keyboard.recognitionFailed", comment: ""))
            onEvent?(.recognitionFailed)
        }
    }

    fileprivate func receive(_ body: Any) {
        guard let message = body as? [String: Any], let type = message["type"] as? String else { return }
        switch type {
        case "ready":
            ready = true
            if wanted { beginPage() }
        case "started":
            guard wanted else { return }
            started = true
            log.info("capture started at \(message["sampleRate"] as? Double ?? 0) Hz")
            onEvent?(.captureStarted)
        case "audio":
            guard wanted, started, let base64 = message["data"] as? String,
                  let data = Data(base64Encoded: base64) else { return }
            append(data)
        case "failed", "interrupted":
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
        #if DEBUG
        captured.append(data)
        if let script, frames > 0, samples / 8_000 != (samples - frames) / 8_000 {
            onEvent?(.heard(scriptedText(script), final: false))
        }
        #endif
    }

    #if DEBUG
    /* Debug builds with `SonaScriptedRecognition` set in the shared container hear those
     * words instead of asking the speech service, which the simulator cannot run: one more
     * word for each half second of audio the web view delivers. With
     * `SonaKeyboardKeepCapture` set they also keep what the microphone heard, as
     * keyboard-capture.wav in the shared container, for a recognizer elsewhere to check. */
    private static var scriptedWords: [String]? {
        DictationPreferences.shared.string(forKey: "SonaScriptedRecognition").map { $0.split(separator: " ").map(String.init) }
    }

    private func scriptedText(_ words: [String]) -> String {
        words.prefix(samples / 8_000).joined(separator: " ")
    }

    private func keepCapture() {
        guard DictationPreferences.shared.bool(forKey: "SonaKeyboardKeepCapture"), !captured.isEmpty,
              let folder = try? KeyboardDraftStore.shared().directory
        else { return }
        var wav = Data()
        func put<T: FixedWidthInteger>(_ value: T) { withUnsafeBytes(of: value.littleEndian) { wav.append(contentsOf: $0) } }
        wav.append(contentsOf: Array("RIFF".utf8)); put(UInt32(36 + captured.count))
        wav.append(contentsOf: Array("WAVEfmt ".utf8)); put(UInt32(16)); put(UInt16(1)); put(UInt16(1))
        put(UInt32(Self.sampleRate)); put(UInt32(Self.sampleRate * 2)); put(UInt16(2)); put(UInt16(16))
        wav.append(contentsOf: Array("data".utf8)); put(UInt32(captured.count))
        wav.append(captured)
        try? wav.write(to: folder.appendingPathComponent("keyboard-capture.wav"))
    }
    #endif

    /* The page opens the microphone only when asked, since an AudioContext made earlier
     * can take audio focus or move a Bluetooth route while the keyboard is merely shown.
     * It stops itself when native code stops calling captureHeartbeat, so a keyboard that
     * was suspended or crashed cannot leave the microphone on. */
    private static let page = """
    <!doctype html><html><body><script>
    const targetRate = \(Int(sampleRate));
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
    window.stopCapture = () => stopCapture();
    window.startCapture = async () => {
      stopCapture();
      let stopping = false, stream, context, source, processor, sink, watchdog;
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

extension KeyboardMicrophone: WKUIDelegate, WKNavigationDelegate {
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
        ready = false
        refused("web content process ended")
        webView.loadHTMLString(Self.page, baseURL: Self.origin)
    }
}

#if DEBUG
/// Debug builds log how soon the keyboard showed it was listening after Dictate, how soon
/// the microphone opened, and how many frames the keyboard dropped while dictating.
@MainActor
final class FrameMeter: NSObject {
    private var link: CADisplayLink?
    private var tapped: CFTimeInterval = 0
    private var last: CFTimeInterval = 0
    private var frames = 0
    private var dropped = 0

    func begin() {
        end()
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
        log.info("\(moment, privacy: .public) \(Int((CACurrentMediaTime() - self.tapped) * 1000)) ms after Dictate")
    }

    func end() {
        guard let link else { return }
        link.invalidate()
        self.link = nil
        log.info("dictation drew \(self.frames) frames and dropped \(self.dropped)")
    }

    @objc private func tick(_ link: CADisplayLink) {
        if frames == 0 {
            log.info("listening shown on the frame \(Int((link.targetTimestamp - self.tapped) * 1000)) ms after Dictate")
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

/// The content controller keeps its handler alive; this keeps it from keeping the microphone.
private final class WeakHandler: NSObject, WKScriptMessageHandler {
    weak var target: KeyboardMicrophone?
    init(_ target: KeyboardMicrophone) { self.target = target }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        MainActor.assumeIsolated { target?.receive(message.body) }
    }
}

import Accelerate
import AVFoundation
import Combine
import Foundation
import SwiftUI

/// Something the operator has to be told about a capture they did not choose.
enum RecorderNotice {
    case microphoneOff
    case microphoneUnavailable
    case interrupted
    case notSaved

    var key: LocalizedStringKey {
        switch self {
        case .microphoneOff: return "status.microphoneOff"
        case .microphoneUnavailable: return "status.microphoneUnavailable"
        case .interrupted: return "status.interrupted"
        case .notSaved: return "status.notSaved"
        }
    }
}

/// Recent input loudness, newest last, read from buffers the capture taps already hold.
@MainActor
final class InputLevelMeter: ObservableObject {
    static let count = 64
    @Published private(set) var levels = [Float](repeating: 0, count: count)
    private var lastPush: ContinuousClock.Instant?

    func push(_ level: Float) {
        let now = ContinuousClock.now
        if let lastPush, now - lastPush < .milliseconds(66) { return }
        lastPush = now
        levels.removeFirst()
        levels.append(level)
    }

    func reset() {
        lastPush = nil
        guard levels.contains(where: { $0 > 0 }) else { return }
        levels = [Float](repeating: 0, count: Self.count)
    }

    nonisolated static func level(of buffer: AVAudioPCMBuffer) -> Float {
        guard let samples = buffer.floatChannelData?[0], buffer.frameLength > 0 else { return 0 }
        var rms: Float = 0
        vDSP_rmsqv(samples, 1, &rms, vDSP_Length(buffer.frameLength))
        guard rms > 0 else { return 0 }
        return min(max((20 * log10(rms) + 50) / 50, 0), 1)
    }
}

/// The phone's microphone capture.
///
/// Nothing here starts without `start()` being called from a tap, and every capture is
/// finalised on disk before the caller sees it, so an interruption costs the tail of a
/// recording and never the whole of it.
@MainActor
final class PhoneRecorder: ObservableObject {
    @Published private(set) var isRecording = false
    @Published private(set) var elapsed: TimeInterval = 0
    /// Set when the system, not the operator, decided the outcome.
    @Published var notice: RecorderNotice?
    let meter = InputLevelMeter()

    private let engine = AVAudioEngine()
    private var resampler: PCMResampler?
    private var startedAt: Date?
    private var ticker: Task<Void, Never>?
    private var interruptionObserver: NSObjectProtocol?

    init() {
        interruptionObserver = NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: AVAudioSession.sharedInstance(),
            queue: .main
        ) { [weak self] notification in
            MainActor.assumeIsolated {
                self?.handleInterruption(notification)
            }
        }
    }

    deinit {
        if let interruptionObserver {
            NotificationCenter.default.removeObserver(interruptionObserver)
        }
    }

    static func requestPermission() async -> Bool {
        await withCheckedContinuation { continuation in
            AVAudioApplication.requestRecordPermission { granted in
                continuation.resume(returning: granted)
            }
        }
    }

    static var hasPermission: Bool {
        AVAudioApplication.shared.recordPermission == .granted
    }

    func start() throws {
        guard !isRecording else { return }
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .default)
        try session.setActive(true, options: [])
        let input = engine.inputNode
        let format = input.inputFormat(forBus: 0)
        let url = FileManager.default.temporaryDirectory
            .appending(path: "capture-\(UUID().uuidString).pcm")
        let resampler = try PCMResampler(url: url, inputFormat: format)
        self.resampler = resampler
        /* The tap runs on a render thread and is the only writer until `stop()` removes
         * it, which is also what drains the last callback before `finish()` reads. */
        let meter = self.meter
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, _ in
            try? resampler.append(buffer)
            let level = InputLevelMeter.level(of: buffer)
            Task { @MainActor in
                guard self?.isRecording == true else { return }
                meter.push(level)
            }
        }
        engine.prepare()
        try engine.start()
        startedAt = Date()
        elapsed = 0
        isRecording = true
        notice = nil
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 200_000_000)
                guard let self, let startedAt = self.startedAt else { return }
                self.elapsed = Date().timeIntervalSince(startedAt)
            }
        }
    }

    /// Stop and hand over the finished bytes with the wall-clock start of the capture.
    func stop() -> (audio: CapturedAudio, recordedAtUtcMs: Int64)? {
        guard isRecording, let resampler, let startedAt else { return nil }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        ticker?.cancel()
        ticker = nil
        isRecording = false
        meter.reset()
        self.resampler = nil
        self.startedAt = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: [])
        guard let audio = try? resampler.finish(), audio.byteLength > 0 else { return nil }
        return (audio, Int64(startedAt.timeIntervalSince1970 * 1000))
    }

    /// A call or another app taking the input ends the capture rather than pausing it:
    /// what was recorded is kept, and starting again stays an explicit tap.
    private func handleInterruption(_ notification: Notification) {
        let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt
        guard let raw, AVAudioSession.InterruptionType(rawValue: raw) == .began,
              isRecording
        else { return }
        notice = .interrupted
        onInterrupted?()
    }

    /// Set by the owner so an interrupted capture still reaches the queue.
    var onInterrupted: (() -> Void)?
}

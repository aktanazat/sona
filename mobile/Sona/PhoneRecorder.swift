import Accelerate
import ActivityKit
import AVFoundation
import Combine
import Foundation
import SwiftUI

/// Something the operator has to be told about a capture they did not choose.
enum RecorderNotice {
    case microphoneOff
    case microphoneUnavailable
    case interrupted
    case activityClosed
    case notSaved

    var key: LocalizedStringKey {
        switch self {
        case .microphoneOff: return "status.microphoneOff"
        case .microphoneUnavailable: return "status.microphoneUnavailable"
        case .interrupted: return "status.interrupted"
        case .activityClosed: return "status.activityClosed"
        case .notSaved: return "status.notSaved"
        }
    }
}

/// The system refused the Live Activity a recording started from a press needs.
struct LiveActivityRefused: Error {}

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
    /// Moments bookmarked in the running recording, in milliseconds from its start.
    @Published private(set) var bookmarks: [Int64] = []
    let meter = InputLevelMeter()

    private let engine = AVAudioEngine()
    private var resampler: PCMResampler?
    private var startedAt: Date?
    private var ticker: Task<Void, Never>?
    private var interruptionObserver: NSObjectProtocol?
    private var activity: Activity<RecordingActivityAttributes>?

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
        /* A recording ends with the process that made it, so a Live Activity found at
         * launch belongs to a run that never stopped. */
        for stale in Activity<RecordingActivityAttributes>.activities {
            Task { await stale.end(nil, dismissalPolicy: .immediate) }
        }
        RecordingControlState.set(false)
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

    /// A recording started by a press outside the app runs only while a Live Activity
    /// shows it, so that start fails when the system refuses one.
    func start(requiringActivity: Bool = false) throws {
        guard !isRecording else { return }
        let startedAt = Date()
        let activity = try? Activity.request(
            attributes: RecordingActivityAttributes(startedAt: startedAt),
            content: activityContent(bookmarks: 0)
        )
        if requiringActivity && activity == nil { throw LiveActivityRefused() }
        do {
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
        } catch {
            if let activity { Task { await activity.end(nil, dismissalPolicy: .immediate) } }
            throw error
        }
        self.startedAt = startedAt
        elapsed = 0
        bookmarks = []
        isRecording = true
        notice = nil
        self.activity = activity
        if requiringActivity, let activity { stopWhenClosed(activity) }
        ticker = Task { [weak self] in
            var renewedAt = startedAt
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 200_000_000)
                guard let self, let startedAt = self.startedAt else { return }
                let now = Date()
                self.elapsed = now.timeIntervalSince(startedAt)
                if now.timeIntervalSince(renewedAt) >= Self.activityRenewal {
                    renewedAt = now
                    self.updateActivity()
                }
            }
        }
        RecordingControlState.set(true)
    }

    /* A Live Activity outlives the process that started it, so it goes stale shortly
     * after the recording stops renewing it, as it does when Sona crashes or is killed.
     * The system draws it stale about two minutes after the last update whatever the
     * stale date says (measured on the simulator), so a shorter lease buys nothing. */
    private static let activityLease: TimeInterval = 60
    private static let activityRenewal: TimeInterval = 30

    private func activityContent(bookmarks: Int) -> ActivityContent<RecordingActivityAttributes.ContentState> {
        ActivityContent(
            state: RecordingActivityAttributes.ContentState(bookmarks: bookmarks),
            staleDate: Date().addingTimeInterval(Self.activityLease)
        )
    }

    private func updateActivity() {
        guard let activity else { return }
        let content = activityContent(bookmarks: bookmarks.count)
        Task { await activity.update(content) }
    }

    private func stopWhenClosed(_ activity: Activity<RecordingActivityAttributes>) {
        Task { [weak self] in
            for await state in activity.activityStateUpdates where state == .dismissed || state == .ended {
                guard let self, self.isRecording, self.activity?.id == activity.id else { return }
                self.notice = .activityClosed
                self.onInterrupted?()
                return
            }
        }
    }

    /// Bookmark this moment of the running recording.
    func bookmark() {
        guard isRecording, let startedAt else { return }
        bookmarks.append(Int64(max(0, Date().timeIntervalSince(startedAt)) * 1000))
        updateActivity()
    }

    /// Stop and hand over the finished bytes with the wall-clock start of the capture and
    /// what was bookmarked in it.
    func stop() -> (audio: CapturedAudio, recordedAtUtcMs: Int64, bookmarkOffsetsMs: [Int64])? {
        guard isRecording, let resampler, let startedAt else { return nil }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        ticker?.cancel()
        ticker = nil
        isRecording = false
        meter.reset()
        self.resampler = nil
        self.startedAt = nil
        if let activity {
            self.activity = nil
            Task { await activity.end(nil, dismissalPolicy: .immediate) }
        }
        RecordingControlState.set(false)
        try? AVAudioSession.sharedInstance().setActive(false, options: [])
        guard let audio = try? resampler.finish(), audio.byteLength > 0 else { return nil }
        return (audio, Int64(startedAt.timeIntervalSince1970 * 1000), bookmarks)
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

import AVFoundation
import Combine
import Foundation
import SwiftUI
import WatchConnectivity

/// The watch records compact AAC and hands the file to the phone.
///
/// It holds no vault key and speaks no vault protocol: the phone encrypts,
/// so a lost or stolen watch carries nothing but its own unsent recordings.
@MainActor
final class WatchRecorder: NSObject, ObservableObject {
    @Published private(set) var isRecording = false
    @Published private(set) var elapsed: TimeInterval = 0
    @Published private(set) var status: LocalizedStringKey = "watch.ready"

    private var recorder: AVAudioRecorder?
    private var startedAtUtcMs: Int64 = 0
    private var ticker: Task<Void, Never>?

    override init() {
        super.init()
        if WCSession.isSupported() {
            WCSession.default.delegate = self
            WCSession.default.activate()
        }
    }

    func toggle() {
        if isRecording {
            stop()
        } else {
            Task { await start() }
        }
    }

    private func start() async {
        guard await requestPermission() else {
            status = "watch.microphoneOff"
            return
        }
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.record, mode: .default)
            try session.setActive(true, options: [])
            /* Kept outside the temporary folder, which the system may empty while the
             * phone is still out of reach and the transfer waits. */
            let folder = try FileManager.default.url(
                for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true
            )
            let url = folder.appending(path: "watch-\(UUID().uuidString).aac")
            let recorder = try AVAudioRecorder(
                url: url,
                settings: [
                    AVFormatIDKey: Int(kAudioFormatMPEG4AAC),
                    AVSampleRateKey: 16000,
                    AVNumberOfChannelsKey: 1,
                    AVEncoderBitRateKey: 24_000,
                    AVEncoderBitRateStrategyKey: AVAudioBitRateStrategy_Constant,
                ]
            )
            guard recorder.record() else {
                status = "watch.microphoneUnavailable"
                return
            }
            self.recorder = recorder
            startedAtUtcMs = Int64(Date().timeIntervalSince1970 * 1000)
            elapsed = 0
            isRecording = true
            status = "watch.recording"
            ticker = Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(nanoseconds: 250_000_000)
                    guard let self, let recorder = self.recorder else { return }
                    self.elapsed = recorder.currentTime
                }
            }
        } catch {
            status = "watch.microphoneUnavailable"
        }
    }

    private func stop() {
        guard let recorder else { return }
        let url = recorder.url
        let durationMs = Int64(recorder.currentTime * 1000)
        recorder.stop()
        self.recorder = nil
        ticker?.cancel()
        ticker = nil
        isRecording = false
        try? AVAudioSession.sharedInstance().setActive(false, options: [])
        WCSession.default.transferFile(
            url,
            metadata: [
                "recorded_at_utc_ms": NSNumber(value: startedAtUtcMs),
                "duration_ms": NSNumber(value: durationMs),
                "audio_codec": "aac_adts",
            ]
        )
        status = "watch.sent"
    }

    private func requestPermission() async -> Bool {
        await withCheckedContinuation { continuation in
            AVAudioApplication.requestRecordPermission { granted in
                continuation.resume(returning: granted)
            }
        }
    }
}

extension WatchRecorder: WCSessionDelegate {
    nonisolated func session(
        _ session: WCSession,
        activationDidCompleteWith activationState: WCSessionActivationState,
        error: Error?
    ) {}

    /// The transfer outlives the recording view, so a failure is reported on the next
    /// launch rather than lost. A failed transfer is not tried again, so its file goes too.
    nonisolated func session(
        _ session: WCSession, didFinish fileTransfer: WCSessionFileTransfer, error: Error?
    ) {
        try? FileManager.default.removeItem(at: fileTransfer.file.fileURL)
        guard error != nil else { return }
        Task { @MainActor [weak self] in
            self?.status = "watch.noPhone"
        }
    }
}

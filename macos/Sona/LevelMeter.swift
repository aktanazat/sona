import AVFoundation
import Observation

/// The microphone level while a recording is on: the last eleven readings,
/// oldest first, each 0 to 1. About forty readings arrive a second. The pill
/// draws them as they are; nothing here is smoothed or animated.
@MainActor
@Observable
final class LevelMeter {
    static let width = 11

    private(set) var history = [Double](repeating: 0, count: LevelMeter.width)
    private let engine = AVAudioEngine()
    private var running = false
    /// True once the tap is installed. `engine.inputNode` must not be touched
    /// before that: creating it with the microphone permission still undecided
    /// blocks the main thread inside CoreAudio until the prompt is answered.
    private var tapped = false

    func start() {
        guard !running else { return }
        running = true
        AVCaptureDevice.requestAccess(for: .audio) { granted in
            Task { @MainActor [weak self] in
                guard granted, let self, self.running else { return }
                self.tap()
            }
        }
    }

    func stop() {
        guard running else { return }
        running = false
        history = [Double](repeating: 0, count: Self.width)
        guard tapped else { return }
        tapped = false
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
    }

    private func tap() {
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
            guard let samples = buffer.floatChannelData?[0] else { return }
            let count = Int(buffer.frameLength)
            guard count > 0 else { return }
            var sum: Float = 0
            for index in 0..<count {
                sum += samples[index] * samples[index]
            }
            let decibels = 20 * log10(max(sqrt(sum / Float(count)), 1e-7))
            // -50 dB is silence in a quiet room; 0 dB is the loudest the input carries.
            let level = Double(min(max((decibels + 50) / 50, 0), 1))
            Task { @MainActor [weak self] in
                self?.push(level)
            }
        }
        tapped = true
        do {
            try engine.start()
        } catch {
            running = false
            tapped = false
            input.removeTap(onBus: 0)
        }
    }

    private func push(_ level: Double) {
        guard running else { return }
        history.removeFirst()
        history.append(level)
    }
}

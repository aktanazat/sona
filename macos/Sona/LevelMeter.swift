import Observation

/// The microphone level while a recording is on: the last eleven readings,
/// oldest first, each 0 to 1. The readings are the core's, from the same
/// input and channel it records, about twenty-four a second. The pill draws
/// them as they are; nothing here is smoothed or animated.
@MainActor
@Observable
final class LevelMeter {
    static let width = 11

    private(set) var history = [Double](repeating: 0, count: LevelMeter.width)
    private var running = false

    func start() {
        running = true
    }

    func stop() {
        running = false
        history = [Double](repeating: 0, count: Self.width)
    }

    /// One frame from the core: sixteen frequency buckets, each 0 to 1. The
    /// pill has one bar per moment, not per band, so the loudest band is
    /// the reading; a voice lights a few bands, and a bar should show it.
    func push(_ buckets: [Float]) {
        guard running, let peak = buckets.max() else { return }
        history.removeFirst()
        history.append(Double(min(max(peak, 0), 1)))
    }
}

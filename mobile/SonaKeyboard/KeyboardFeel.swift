import QuartzCore
import os

/// How the keyboard feels, in numbers Instruments and `log stream` can read: the time from a
/// finger lifting to the text being handed to the field, and the frames a gesture dropped.
/// Debug builds only; a release keyboard neither logs nor runs the frame counter.
@MainActor
enum KeyboardFeel {
    /// `since` is the touch's own timestamp, on the same clock as `CACurrentMediaTime`.
    static func typed(since: TimeInterval) {
        #if DEBUG
        let ms = (CACurrentMediaTime() - since) * 1000
        log.debug("key-to-text \(ms, format: .fixed(precision: 2), privacy: .public) ms")
        #endif
    }

    static func touching(_ active: Bool) {
        #if DEBUG
        if active { meter.start() } else { meter.stop() }
        #endif
    }

    #if DEBUG
    private static let log = Logger(subsystem: "com.aktanazat.sona.mobile.keyboard", category: "feel")
    private static let meter = FrameMeter()

    /// Counts frames that came later than the display asked for while a finger is down.
    private final class FrameMeter {
        private var link: CADisplayLink?
        private var last: CFTimeInterval = 0
        private var frames = 0
        private var dropped = 0
        private var worst: CFTimeInterval = 0
        private var stopping: DispatchWorkItem?

        func start() {
            stopping?.cancel()
            stopping = nil
            guard link == nil else { return }
            frames = 0
            dropped = 0
            worst = 0
            last = 0
            let link = CADisplayLink(target: Target(meter: self), selector: #selector(Target.tick(_:)))
            link.preferredFrameRateRange = CAFrameRateRange(minimum: 60, maximum: 120, preferred: 120)
            link.add(to: .main, forMode: .common)
            self.link = link
        }

        /// Keeps counting a moment after the last finger lifts, so the release animation counts too.
        func stop() {
            let work = DispatchWorkItem { [weak self] in self?.finish() }
            stopping = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: work)
        }

        private func finish() {
            link?.invalidate()
            link = nil
            guard frames > 0 else { return }
            KeyboardFeel.log.debug(
                "frames \(self.frames, privacy: .public) dropped \(self.dropped, privacy: .public) worst \(self.worst * 1000, format: .fixed(precision: 1), privacy: .public) ms"
            )
        }

        fileprivate func tick(_ link: CADisplayLink) {
            defer { last = link.timestamp }
            guard last > 0 else { return }
            let gap = link.timestamp - last
            let expected = link.targetTimestamp - link.timestamp
            frames += 1
            worst = max(worst, gap)
            if expected > 0, gap > expected * 1.5 { dropped += Int((gap / expected).rounded()) - 1 }
        }

        private final class Target: NSObject {
            weak var meter: FrameMeter?
            init(meter: FrameMeter) { self.meter = meter }
            @objc func tick(_ link: CADisplayLink) {
                MainActor.assumeIsolated { meter?.tick(link) }
            }
        }
    }
    #endif
}

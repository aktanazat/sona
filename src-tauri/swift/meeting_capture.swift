import AppKit
import ApplicationServices
import AudioToolbox
import CoreAudio
import CoreGraphics
import CoreMedia
import Dispatch
import Foundation
import ScreenCaptureKit

private enum BridgeResult: Int32 {
    case ok = 0
    case invalidArgument = 1
    case unsupported = 2
    case permissionDenied = 3
    case sourceUnavailable = 4
    case routeUnavailable = 5
    case streamFailure = 6
}

private enum FailureCategory: Int32 {
    case permission = 1
    case source = 2
    case route = 3
    case stream = 4
}

private enum FailureCode: Int32 {
    case unsupportedOS = 1
    case screenRecordingDenied = 2
    case missingEntitlement = 3
    case noDisplay = 4
    case noMatchingApplication = 5
    case selectedApplicationExited = 6
    case audioFormatChanged = 7
    case audioBufferNotContiguous = 8
    case invalidTimestamp = 9
    case streamStoppedUnexpectedly = 10
    case bridgeArgumentInvalid = 11
    case clockUnavailable = 12
    case timestampDiscontinuity = 13
}

private let requestedSampleRate: UInt32 = 48_000
private let requestedChannelCount: UInt32 = 2
private let maximumBundleIDs = 64
private let maximumBundleIDCharacters = 255
private let maximumEvidenceTitleCharacters = 160
private let maximumEvidenceHostCharacters = 253
private let accessibilityReadTimeoutSeconds: Float = 0.05
/// The widest interleaved layout this bridge will assemble. ScreenCaptureKit is
/// configured for two channels; the ceiling is what bounds the scratch buffer
/// and the buffer list a renegotiated stream can ask for.
private let maximumChannelCount: UInt32 = 8
/// One second at that ceiling and 48 kHz, so a sample buffer claiming an
/// impossible frame count cannot turn into an impossible allocation.
private let maximumScratchSamples = 48_000 * Int(maximumChannelCount)

private let packetTimestampReset: UInt32 = 1
private let packetSourceRestarted: UInt32 = 1 << 2
private let packetFormatChanged: UInt32 = 1 << 3

public typealias PacketCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafePointer<Float>?,
    UInt,
    UInt32,
    UInt32,
    Int64,
    Int32,
    UInt64,
    UInt64,
    UInt64,
    UInt32
) -> Void

public typealias StatusCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    Int32,
    Int32,
    UInt64,
    UInt64,
    Int64,
    Int32,
    UInt64,
    UInt32
) -> Void

public typealias SuggestionCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    UnsafePointer<CChar>?,
    UnsafePointer<CChar>?,
    UnsafePointer<CChar>?,
    UInt32,
    UInt64
) -> Void

private final class Completion: @unchecked Sendable {
    private let semaphore = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var result: Int32 = BridgeResult.streamFailure.rawValue

    func resolve(_ result: Int32) {
        lock.lock()
        self.result = result
        lock.unlock()
        semaphore.signal()
    }

    func wait() -> Int32 {
        semaphore.wait()
        lock.lock()
        defer { lock.unlock() }
        return result
    }
}
private struct RawTimestamp: Equatable, Sendable {
    let value: Int64
    let timescale: Int32
}

private struct PacketFormat: Equatable {
    let sampleRateHz: UInt32
    let channels: UInt32
}

/// What one audio format means to this bridge: the packet fields it produces,
/// and whether its samples arrive one plane per channel. `nil` is a format the
/// bridge cannot read at all.
private struct AudioPacketLayout: Equatable {
    let format: PacketFormat
    let planar: Bool
}

/// A planar description counts one channel's frame in `mBytesPerFrame` and sets
/// `kAudioFormatFlagIsNonInterleaved`. ScreenCaptureKit delivers exactly that,
/// so a guard demanding the interleaved shape rejected every buffer the stream
/// ever produced. Branch on the flag, and reject only what cannot be read.
private func audioPacketLayout(_ format: AudioStreamBasicDescription) -> AudioPacketLayout? {
    let flags = format.mFormatFlags
    let channels = format.mChannelsPerFrame
    let planar = (flags & kAudioFormatFlagIsNonInterleaved) != 0
    let bytesPerFrame = UInt32(MemoryLayout<Float>.size)
        .multipliedReportingOverflow(by: planar ? 1 : channels)
    guard format.mSampleRate.isFinite,
          format.mSampleRate > 0,
          format.mSampleRate <= Double(UInt32.max),
          format.mSampleRate.rounded(.towardZero) == format.mSampleRate,
          channels > 0,
          channels <= maximumChannelCount,
          !bytesPerFrame.overflow,
          format.mFormatID == kAudioFormatLinearPCM,
          format.mBitsPerChannel == 32,
          format.mFramesPerPacket == 1,
          format.mBytesPerFrame == bytesPerFrame.partialValue,
          format.mBytesPerPacket == bytesPerFrame.partialValue,
          (flags & kAudioFormatFlagIsFloat) != 0
    else {
        return nil
    }
    return AudioPacketLayout(
        format: PacketFormat(
            sampleRateHz: UInt32(format.mSampleRate),
            channels: channels
        ),
        planar: planar
    )
}

/// Copies `planes` into `destination` as interleaved frames, which is the only
/// shape the packet callback accepts. Planar input holds one buffer per
/// channel; interleaved input holds one buffer of whole frames. False means the
/// buffer list did not match the layout its format promised.
private func interleaveAudio(
    planes: UnsafeMutableAudioBufferListPointer,
    frameCount: Int,
    channels: Int,
    planar: Bool,
    into destination: UnsafeMutablePointer<Float>
) -> Bool {
    let planeCount = planar ? channels : 1
    let planeFloats = planar ? frameCount : frameCount * channels
    guard planes.count == planeCount else {
        return false
    }
    let planeBytes = planeFloats * MemoryLayout<Float>.size
    for plane in 0..<planeCount {
        let buffer = planes[plane]
        guard buffer.mNumberChannels == UInt32(planar ? 1 : channels),
              Int(buffer.mDataByteSize) == planeBytes,
              let data = buffer.mData
        else {
            return false
        }
        let source = data.assumingMemoryBound(to: Float.self)
        if planar {
            for frame in 0..<frameCount {
                destination[frame * channels + plane] = source[frame]
            }
        } else {
            destination.update(from: source, count: planeFloats)
        }
    }
    return true
}

private struct StreamClockBridge: Sendable {
    let nativeAnchor: RawTimestamp
    let hostMonotonicAnchorNs: UInt64
    let sessionOffsetNs: UInt64
    let formatEpoch: UInt64
}

private final class StartCompletion: @unchecked Sendable {
    private let semaphore = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var result: Int32 = BridgeResult.streamFailure.rawValue
    private var bridge: StreamClockBridge?

    func resolve(_ result: Int32, bridge: StreamClockBridge?) {
        lock.lock()
        self.result = result
        self.bridge = bridge
        lock.unlock()
        semaphore.signal()
    }

    func wait() -> (Int32, StreamClockBridge?) {
        semaphore.wait()
        lock.lock()
        defer { lock.unlock() }
        return (result, bridge)
    }
}

private func monotonicNowNs() -> UInt64 {
    DispatchTime.now().uptimeNanoseconds
}

private func nativeTimestampNs(_ time: CMTime) -> UInt64? {
    guard time.isValid, !time.isIndefinite, time.value >= 0, time.timescale > 0 else {
        return nil
    }

    let nanoseconds = CMTimeConvertScale(time, timescale: 1_000_000_000, method: .roundTowardZero)
    guard nanoseconds.isValid, !nanoseconds.isIndefinite, nanoseconds.value >= 0 else {
        return nil
    }
    return UInt64(nanoseconds.value)
}

private func hostClockNowNs() -> UInt64? {
    nativeTimestampNs(CMClockGetTime(CMClockGetHostTimeClock()))
}

private func rawTimestamp(_ time: CMTime) -> RawTimestamp? {
    guard time.isValid, !time.isIndefinite, time.timescale > 0 else {
        return nil
    }
    return RawTimestamp(value: time.value, timescale: time.timescale)
}

private func boundedBundleIDs(
    _ rawBundleIDs: UnsafeRawPointer?,
    count: UInt
) -> [String]? {
    guard count <= UInt(maximumBundleIDs) else {
        return nil
    }
    guard count == 0 || rawBundleIDs != nil else {
        return nil
    }

    let pointers = rawBundleIDs?.assumingMemoryBound(to: UnsafePointer<CChar>?.self)
    var result: [String] = []
    result.reserveCapacity(Int(count))

    for index in 0..<Int(count) {
        guard let pointer = pointers?[index],
              let bundleID = String(validatingUTF8: pointer),
              !bundleID.isEmpty,
              bundleID.count <= maximumBundleIDCharacters
        else {
            return nil
        }
        result.append(bundleID.lowercased())
    }

    return Array(Set(result)).sorted()
}

private func classifyStreamError(_ error: Error) -> (FailureCategory, Int32, Int32) {
    let nsError = error as NSError
    let code = Int32(clamping: nsError.code)

    if nsError.domain == SCStreamErrorDomain {
        switch nsError.code {
        case -3801:
            return (.permission, FailureCode.screenRecordingDenied.rawValue, BridgeResult.permissionDenied.rawValue)
        case -3803:
            return (.permission, FailureCode.missingEntitlement.rawValue, BridgeResult.permissionDenied.rawValue)
        case -3806:
            return (.route, FailureCode.noMatchingApplication.rawValue, BridgeResult.routeUnavailable.rawValue)
        case -3813, -3814, -3815:
            return (.source, FailureCode.noDisplay.rawValue, BridgeResult.sourceUnavailable.rawValue)
        default:
            return (.stream, code, BridgeResult.streamFailure.rawValue)
        }
    }

    return (.stream, code, BridgeResult.streamFailure.rawValue)
}

private func withOptionalCString<T>(_ value: String?, _ body: (UnsafePointer<CChar>?) -> T) -> T {
    guard let value else {
        return body(nil)
    }
    return value.withCString(body)
}

@available(macOS 14.0, *)
private final class CaptureBridge: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let packetCallback: PacketCallback
    private let statusCallback: StatusCallback
    private let callbackContext: UnsafeMutableRawPointer?
    private let requestedBundleIDs: Set<String>
    private let ownBundleID: String
    private let ownProcessID: pid_t
    private let outputQueue = DispatchQueue(
        label: "computer.sona.meeting.system-audio-output",
        qos: .userInitiated
    )
    private let outputQueueKey = DispatchSpecificKey<UInt8>()
    private let controlQueue = DispatchQueue(label: "computer.sona.meeting.system-audio-control")

    private var stream: SCStream?
    private var filter: SCContentFilter?
    private var configuration: SCStreamConfiguration?
    private var selectedRouteProcessIDs: Set<pid_t> = []
    private var routeObserver: NSObjectProtocol?
    private var isCapturing = false

    // Accessed exclusively from outputQueue.
    private var sourceEpoch: UInt64 = 0
    private var formatEpoch: UInt64 = 1
    private var lastPresentationTimestamp: CMTime?
    private var activeFormat: PacketFormat?
    private var pendingSourceRestart = false
    private var callbacksEnabled = false
    private var unsupportedFormat = false
    /// Interleaved staging for one sample buffer. ScreenCaptureKit delivers
    /// planar Float32, and the packet callback's contract is interleaved, so
    /// every buffer is assembled here before it crosses the boundary. It grows
    /// only when a stream first delivers a larger buffer than any before it.
    private var scratch: UnsafeMutablePointer<Float>?
    private var scratchCapacity = 0
    private let bufferList = AudioBufferList.allocate(maximumBuffers: Int(maximumChannelCount))

    init(
        requestedBundleIDs: [String],
        epoch: UInt64,
        packetCallback: @escaping PacketCallback,
        statusCallback: @escaping StatusCallback,
        callbackContext: UnsafeMutableRawPointer?
    ) {
        self.packetCallback = packetCallback
        self.statusCallback = statusCallback
        self.callbackContext = callbackContext
        self.requestedBundleIDs = Set(requestedBundleIDs)
        self.ownBundleID = (Bundle.main.bundleIdentifier ?? "").lowercased()
        self.ownProcessID = ProcessInfo.processInfo.processIdentifier
        super.init()

        outputQueue.setSpecific(key: outputQueueKey, value: 1)
        outputQueue.sync {
            callbacksEnabled = true
            sourceEpoch = epoch
            formatEpoch = 1
            lastPresentationTimestamp = nil
            activeFormat = nil
            pendingSourceRestart = false
            unsupportedFormat = false
        }

    }

    deinit {
        removeRouteObserver()
        scratch?.deallocate()
        free(bufferList.unsafeMutablePointer)
    }

    func startSynchronously(sessionHostAnchorNs: UInt64) -> (Int32, StreamClockBridge?) {
        let completion = StartCompletion()
        prepareAndStart(sessionHostAnchorNs: sessionHostAnchorNs) { result, bridge in
            completion.resolve(result, bridge: bridge)
        }
        return completion.wait()
    }

    func pauseSynchronously() -> Int32 {
        removeRouteObserver()
        guard let stream, isCapturing else {
            return BridgeResult.ok.rawValue
        }

        let completion = Completion()
        stream.stopCapture { [weak self] error in
            self?.controlQueue.async {
                guard let self else {
                    completion.resolve(BridgeResult.streamFailure.rawValue)
                    return
                }
                if let error {
                    let (category, failureCode, result) = classifyStreamError(error)
                    self.reportFailure(category, failureCode)
                    completion.resolve(result)
                    return
                }
                self.isCapturing = false
                self.drainOutputQueue()
                completion.resolve(BridgeResult.ok.rawValue)
            }
        }
        return completion.wait()
    }

    func resumeSynchronously(
        epoch: UInt64,
        sessionHostAnchorNs: UInt64
    ) -> (Int32, StreamClockBridge?) {
        guard let stream, !isCapturing else {
            return (BridgeResult.streamFailure.rawValue, nil)
        }

        outputQueue.sync {
            sourceEpoch = epoch
            formatEpoch &+= 1
            lastPresentationTimestamp = nil
            activeFormat = nil
            pendingSourceRestart = true
            unsupportedFormat = false
        }

        let completion = StartCompletion()
        stream.startCapture { [weak self] error in
            self?.controlQueue.async {
                guard let self else {
                    completion.resolve(BridgeResult.streamFailure.rawValue, bridge: nil)
                    return
                }
                if let error {
                    let (category, failureCode, result) = classifyStreamError(error)
                    self.reportFailure(category, failureCode)
                    completion.resolve(result, bridge: nil)
                    return
                }
                guard let bridge = self.clockBridge(sessionHostAnchorNs: sessionHostAnchorNs) else {
                    self.reportFailure(.stream, FailureCode.clockUnavailable.rawValue)
                    completion.resolve(BridgeResult.streamFailure.rawValue, bridge: nil)
                    return
                }
                self.isCapturing = true
                self.installRouteObserver()
                completion.resolve(BridgeResult.ok.rawValue, bridge: bridge)
            }
        }
        return completion.wait()
    }

    func stopSynchronously() -> Int32 {
        removeRouteObserver()

        let result: Int32
        if let stream, isCapturing {
            let completion = Completion()
            stream.stopCapture { [weak self] error in
                self?.controlQueue.async {
                    guard let self else {
                        completion.resolve(BridgeResult.streamFailure.rawValue)
                        return
                    }
                    if let error {
                        let (category, failureCode, result) = classifyStreamError(error)
                        self.reportFailure(category, failureCode)
                        completion.resolve(result)
                        return
                    }
                    self.isCapturing = false
                    self.drainOutputQueue()
                    completion.resolve(BridgeResult.ok.rawValue)
                }
            }
            result = completion.wait()
        } else {
            result = BridgeResult.ok.rawValue
        }

        tearDownStream()
        return result
    }

    func abortSynchronously() -> Int32 {
        stopSynchronously()
    }

    func tearDownStream() {
        removeRouteObserver()
        disableCallbacksAndDrain()
        if let stream {
            do {
                try stream.removeStreamOutput(self, type: .audio)
            } catch {
                let (category, failureCode, _) = classifyStreamError(error)
                reportFailure(category, failureCode)
            }
        }
        stream = nil
        filter = nil
        configuration = nil
        selectedRouteProcessIDs.removeAll()
        isCapturing = false
        drainOutputQueue()
    }

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .audio, callbacksEnabled else {
            return
        }

        guard let hostMonotonicAnchorNs = hostClockNowNs() else {
            reportFailure(.stream, FailureCode.clockUnavailable.rawValue)
            return
        }
        let presentationTime = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        guard let timestamp = rawTimestamp(presentationTime) else {
            reportFailure(
                .route,
                FailureCode.invalidTimestamp.rawValue,
                hostMonotonicAnchorNs: hostMonotonicAnchorNs
            )
            return
        }

        guard let formatDescription = CMSampleBufferGetFormatDescription(sampleBuffer),
              let streamDescription = CMAudioFormatDescriptionGetStreamBasicDescription(formatDescription)
        else {
            reportFailure(
                .route,
                FailureCode.audioFormatChanged.rawValue,
                timestamp: timestamp,
                hostMonotonicAnchorNs: hostMonotonicAnchorNs
            )
            return
        }

        guard let layout = audioPacketLayout(streamDescription.pointee) else {
            reportUnsupportedFormat(
                timestamp: timestamp,
                hostMonotonicAnchorNs: hostMonotonicAnchorNs,
                frames: UInt32(clamping: CMSampleBufferGetNumSamples(sampleBuffer))
            )
            return
        }
        unsupportedFormat = false
        let packetFormat = layout.format
        var flags: UInt32 = 0
        if let activeFormat, activeFormat != packetFormat {
            sourceEpoch &+= 1
            formatEpoch &+= 1
            flags |= packetFormatChanged | packetSourceRestarted
            reportFailure(
                .route,
                FailureCode.audioFormatChanged.rawValue,
                timestamp: timestamp,
                hostMonotonicAnchorNs: hostMonotonicAnchorNs
            )
        }
        activeFormat = packetFormat
        if let lastPresentationTimestamp,
           (lastPresentationTimestamp.timescale != presentationTime.timescale
                || CMTimeCompare(presentationTime, lastPresentationTimestamp) <= 0)
        {
            sourceEpoch &+= 1
            flags |= packetTimestampReset | packetSourceRestarted
            reportFailure(
                .route,
                FailureCode.timestampDiscontinuity.rawValue,
                timestamp: timestamp,
                hostMonotonicAnchorNs: hostMonotonicAnchorNs
            )
        }
        if pendingSourceRestart {
            flags |= packetSourceRestarted
            pendingSourceRestart = false
        }
        lastPresentationTimestamp = presentationTime

        let frameCount = CMSampleBufferGetNumSamples(sampleBuffer)
        guard frameCount > 0 else {
            return
        }
        guard let samples = interleavedSamples(
            sampleBuffer,
            frameCount: frameCount,
            channels: Int(packetFormat.channels),
            planar: layout.planar
        ) else {
            reportFailure(
                .route,
                FailureCode.audioBufferNotContiguous.rawValue,
                timestamp: timestamp,
                hostMonotonicAnchorNs: hostMonotonicAnchorNs,
                frames: UInt32(clamping: frameCount)
            )
            return
        }

        packetCallback(
            callbackContext,
            samples,
            UInt(frameCount),
            packetFormat.sampleRateHz,
            packetFormat.channels,
            timestamp.value,
            timestamp.timescale,
            hostMonotonicAnchorNs,
            sourceEpoch,
            formatEpoch,
            flags
        )
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        let nsError = error as NSError
        if nsError.domain == SCStreamErrorDomain && nsError.code == -3817 {
            return
        }
        let (category, failureCode, _) = classifyStreamError(error)
        reportFailure(category, failureCode)
    }

    private func prepareAndStart(
        sessionHostAnchorNs: UInt64,
        completion: @escaping @Sendable (Int32, StreamClockBridge?) -> Void
    ) {
        guard CGPreflightScreenCaptureAccess() else {
            reportFailure(.permission, FailureCode.screenRecordingDenied.rawValue)
            completion(BridgeResult.permissionDenied.rawValue, nil)
            return
        }

        Task.detached { [weak self] in
            do {
                let content = try await SCShareableContent.excludingDesktopWindows(
                    true,
                    onScreenWindowsOnly: true
                )
                guard let self else {
                    completion(BridgeResult.streamFailure.rawValue, nil)
                    return
                }
                self.configureAndStart(
                    content: content,
                    sessionHostAnchorNs: sessionHostAnchorNs,
                    completion: completion
                )
            } catch {
                guard let self else {
                    completion(BridgeResult.streamFailure.rawValue, nil)
                    return
                }
                let (category, failureCode, result) = classifyStreamError(error)
                self.reportFailure(category, failureCode)
                completion(result, nil)
            }
        }
    }

    private func configureAndStart(
        content: SCShareableContent,
        sessionHostAnchorNs: UInt64,
        completion: @escaping @Sendable (Int32, StreamClockBridge?) -> Void
    ) {
        guard let display = content.displays.first else {
            reportFailure(.source, FailureCode.noDisplay.rawValue)
            completion(BridgeResult.sourceUnavailable.rawValue, nil)
            return
        }

        let candidates = content.applications.filter { application in
            application.processID != ownProcessID && application.bundleIdentifier.lowercased() != ownBundleID
        }
        let applications: [SCRunningApplication]
        let routeProcessIDs: Set<pid_t>

        if requestedBundleIDs.isEmpty {
            guard !candidates.isEmpty else {
                reportFailure(.source, FailureCode.noMatchingApplication.rawValue)
                completion(BridgeResult.sourceUnavailable.rawValue, nil)
                return
            }
            applications = candidates
            routeProcessIDs = []
        } else {
            let matched = candidates.filter { requestedBundleIDs.contains($0.bundleIdentifier.lowercased()) }
            let matchedBundleIDs = Set(matched.map { $0.bundleIdentifier.lowercased() })
            guard matchedBundleIDs == requestedBundleIDs else {
                reportFailure(.route, FailureCode.noMatchingApplication.rawValue)
                completion(BridgeResult.routeUnavailable.rawValue, nil)
                return
            }
            applications = matched
            routeProcessIDs = Set(matched.map(\.processID))
        }

        let filter = SCContentFilter(
            display: display,
            including: applications,
            exceptingWindows: []
        )
        let configuration = SCStreamConfiguration()
        configuration.capturesAudio = true
        configuration.excludesCurrentProcessAudio = true
        configuration.sampleRate = Int(requestedSampleRate)
        configuration.channelCount = Int(requestedChannelCount)

        let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
        do {
            try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: outputQueue)
        } catch {
            let (category, failureCode, result) = classifyStreamError(error)
            reportFailure(category, failureCode)
            completion(result, nil)
            return
        }

        self.filter = filter
        self.configuration = configuration
        self.stream = stream
        self.selectedRouteProcessIDs = routeProcessIDs

        stream.startCapture { [weak self] error in
            guard let self else {
                completion(BridgeResult.streamFailure.rawValue, nil)
                return
            }
            self.controlQueue.async {
                if let error {
                    let (category, failureCode, result) = classifyStreamError(error)
                    self.reportFailure(category, failureCode)
                    self.tearDownStream()
                    completion(result, nil)
                    return
                }
                guard let bridge = self.clockBridge(sessionHostAnchorNs: sessionHostAnchorNs) else {
                    self.reportFailure(.stream, FailureCode.clockUnavailable.rawValue)
                    self.tearDownStream()
                    completion(BridgeResult.streamFailure.rawValue, nil)
                    return
                }
                self.isCapturing = true
                self.installRouteObserver()
                completion(BridgeResult.ok.rawValue, bridge)
            }
        }
    }

    private func clockBridge(sessionHostAnchorNs: UInt64) -> StreamClockBridge? {
        guard let stream,
              let synchronizationClock = stream.synchronizationClock,
              let nativeAnchor = rawTimestamp(CMClockGetTime(synchronizationClock)),
              let hostMonotonicAnchorNs = hostClockNowNs(),
              hostMonotonicAnchorNs >= sessionHostAnchorNs
        else {
            return nil
        }
        return StreamClockBridge(
            nativeAnchor: nativeAnchor,
            hostMonotonicAnchorNs: hostMonotonicAnchorNs,
            sessionOffsetNs: hostMonotonicAnchorNs - sessionHostAnchorNs,
            formatEpoch: outputFormatEpoch()
        )
    }

    private func installRouteObserver() {
        guard routeObserver == nil, !selectedRouteProcessIDs.isEmpty else {
            return
        }

        routeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didTerminateApplicationNotification,
            object: nil,
            queue: nil
        ) { [weak self] notification in
            guard let self,
                  let application = notification.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
                  self.selectedRouteProcessIDs.contains(application.processIdentifier)
            else {
                return
            }
            self.reportFailure(.route, FailureCode.selectedApplicationExited.rawValue)
        }
    }

    private func removeRouteObserver() {
        if let routeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(routeObserver)
            self.routeObserver = nil
        }
    }

    private func drainOutputQueue() {
        if DispatchQueue.getSpecific(key: outputQueueKey) == nil {
            outputQueue.sync {}
        }
    }

    private func outputFormatEpoch() -> UInt64 {
        if DispatchQueue.getSpecific(key: outputQueueKey) != nil {
            return formatEpoch
        }
        return outputQueue.sync { formatEpoch }
    }

    private func disableCallbacksAndDrain() {
        if DispatchQueue.getSpecific(key: outputQueueKey) != nil {
            callbacksEnabled = false
        } else {
            outputQueue.sync {
                callbacksEnabled = false
            }
        }
    }

    private func reportFailure(
        _ category: FailureCategory,
        _ code: Int32,
        timestamp: RawTimestamp? = nil,
        hostMonotonicAnchorNs: UInt64 = 0,
        frames: UInt32 = 0
    ) {
        if DispatchQueue.getSpecific(key: outputQueueKey) != nil {
            reportFailureOnOutputQueue(category, code, timestamp, hostMonotonicAnchorNs, frames)
        } else {
            outputQueue.sync {
                reportFailureOnOutputQueue(category, code, timestamp, hostMonotonicAnchorNs, frames)
            }
        }
    }

    private func reportFailureOnOutputQueue(
        _ category: FailureCategory,
        _ code: Int32,
        _ timestamp: RawTimestamp?,
        _ hostMonotonicAnchorNs: UInt64,
        _ frames: UInt32
    ) {
        guard callbacksEnabled else {
            return
        }
        statusCallback(
            callbackContext,
            category.rawValue,
            code,
            sourceEpoch,
            formatEpoch,
            timestamp?.value ?? 0,
            timestamp?.timescale ?? 0,
            hostMonotonicAnchorNs,
            frames
        )
    }

    /// Copies one sample buffer's audio into `scratch`, interleaving planar
    /// input on the way in, and returns it. The pointer belongs to this bridge
    /// and stays valid until the next buffer arrives on the output queue; the
    /// packet callback copies before it returns.
    private func interleavedSamples(
        _ sampleBuffer: CMSampleBuffer,
        frameCount: Int,
        channels: Int,
        planar: Bool
    ) -> UnsafePointer<Float>? {
        let sampleCount = frameCount.multipliedReportingOverflow(by: channels)
        guard !sampleCount.overflow,
              let scratch = reserveScratch(sampleCount: sampleCount.partialValue)
        else {
            return nil
        }

        var blockBuffer: CMBlockBuffer?
        // CoreMedia wants the size of the list it is about to fill, not the
        // capacity of the one it was handed: a wider list reads as
        // `kCMSampleBufferError_ArrayTooSmall`. The format already said how
        // many planes to expect, so state that size and let a buffer that
        // disagrees with its own format be refused.
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer,
            bufferListSizeNeededOut: nil,
            bufferListOut: bufferList.unsafeMutablePointer,
            bufferListSize: AudioBufferList.sizeInBytes(maximumBuffers: planar ? channels : 1),
            blockBufferAllocator: nil,
            blockBufferMemoryAllocator: nil,
            flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
            blockBufferOut: &blockBuffer
        )
        guard status == noErr,
              blockBuffer != nil,
              interleaveAudio(
                  planes: bufferList,
                  frameCount: frameCount,
                  channels: channels,
                  planar: planar,
                  into: scratch
              )
        else {
            return nil
        }
        return UnsafePointer(scratch)
    }

    private func reserveScratch(sampleCount: Int) -> UnsafeMutablePointer<Float>? {
        guard sampleCount > 0, sampleCount <= maximumScratchSamples else {
            return nil
        }
        if scratchCapacity < sampleCount {
            scratch?.deallocate()
            scratch = UnsafeMutablePointer<Float>.allocate(capacity: sampleCount)
            scratchCapacity = sampleCount
        }
        return scratch
    }

    /// One epoch bump per transition into the unsupported state, not one per
    /// buffer. The epoch pair names which format a packet was captured under,
    /// so bumping it for an unchanging condition wrote a clock-epoch row and a
    /// gap row every 20 ms and starved the other lane's writer. The failure
    /// itself is still reported per buffer, so the frames stay counted.
    private func reportUnsupportedFormat(
        timestamp: RawTimestamp,
        hostMonotonicAnchorNs: UInt64,
        frames: UInt32
    ) {
        if !unsupportedFormat {
            unsupportedFormat = true
            sourceEpoch &+= 1
            formatEpoch &+= 1
            activeFormat = nil
        }
        reportFailure(
            .route,
            FailureCode.audioFormatChanged.rawValue,
            timestamp: timestamp,
            hostMonotonicAnchorNs: hostMonotonicAnchorNs,
            frames: frames
        )
    }
}

private struct AccessibilityEvidence {
    let title: String?
    let urlHost: String?
    let axUnavailable: Bool
}

private func boundedTitle(_ value: String) -> String? {
    guard !value.isEmpty else {
        return nil
    }
    return String(value.prefix(maximumEvidenceTitleCharacters))
}

private func boundedHost(_ url: URL) -> String? {
    guard let host = url.host, !host.isEmpty else {
        return nil
    }
    return String(host.lowercased().prefix(maximumEvidenceHostCharacters))
}

private func copiedAXAttribute(_ element: AXUIElement, _ attribute: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success else {
        return nil
    }
    return value
}

private func accessibilityEvidence(for processID: pid_t) -> AccessibilityEvidence {
    guard AXIsProcessTrusted() else {
        return AccessibilityEvidence(title: nil, urlHost: nil, axUnavailable: true)
    }

    let application = AXUIElementCreateApplication(processID)
    _ = AXUIElementSetMessagingTimeout(application, accessibilityReadTimeoutSeconds)
    guard let focusedWindowValue = copiedAXAttribute(application, kAXFocusedWindowAttribute),
          CFGetTypeID(focusedWindowValue) == AXUIElementGetTypeID()
    else {
        return AccessibilityEvidence(title: nil, urlHost: nil, axUnavailable: false)
    }
    let focusedWindow = unsafeBitCast(focusedWindowValue, to: AXUIElement.self)
    _ = AXUIElementSetMessagingTimeout(focusedWindow, accessibilityReadTimeoutSeconds)
    let title = copiedAXAttribute(focusedWindow, kAXTitleAttribute).flatMap { value in
        (value as? String).flatMap(boundedTitle)
    }
    let urlHost = copiedAXAttribute(focusedWindow, kAXURLAttribute).flatMap { value in
        (value as? URL).flatMap(boundedHost)
    }

    return AccessibilityEvidence(title: title, urlHost: urlHost, axUnavailable: false)
}

private let supportedMeetingBundleIDs: Set<String> = [
    "com.apple.facetime",
    "com.apple.safari",
    "com.cisco.webex",
    "com.cisco.webexmeetingsapp",
    "com.google.chrome",
    "com.google.chrome.canary",
    "com.microsoft.edgemac",
    "com.microsoft.teams",
    "com.microsoft.teams2",
    "com.tinyspeck.slackmacgap",
    "company.thebrowser.browser",
    "org.mozilla.firefox",
    "us.zoom.xos",
]

private let suggestionAXUnavailableFlag: UInt32 = 1

private final class SuggestionObserver: @unchecked Sendable {
    private let callback: SuggestionCallback
    private let callbackContext: UnsafeMutableRawPointer?
    private let observedBundleIDs: Set<String>
    private let evidenceQueue = DispatchQueue(label: "computer.sona.meeting.suggestion-evidence")
    private var token: NSObjectProtocol?

    // Accessed exclusively from evidenceQueue.
    private var isActive = false

    init(
        configuredBundleIDs: [String],
        callback: @escaping SuggestionCallback,
        callbackContext: UnsafeMutableRawPointer?
    ) {
        self.callback = callback
        self.callbackContext = callbackContext
        self.observedBundleIDs = supportedMeetingBundleIDs.union(configuredBundleIDs)
    }

    deinit {
        stop()
    }

    func start() {
        guard token == nil else {
            return
        }

        evidenceQueue.sync {
            isActive = true
        }
        token = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification,
            object: nil,
            queue: nil
        ) { [weak self] notification in
            guard let self,
                  let application = notification.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
                  let bundleID = application.bundleIdentifier,
                  self.observedBundleIDs.contains(bundleID.lowercased())
            else {
                return
            }

            self.evidenceQueue.async { [weak self] in
                self?.emit(bundleID: bundleID, processID: application.processIdentifier)
            }
        }
    }

    func stop() {
        evidenceQueue.sync {
            isActive = false
        }
        if let token {
            NSWorkspace.shared.notificationCenter.removeObserver(token)
            self.token = nil
        }
    }

    /// Reads the frontmost application's focused window now, when that
    /// application is `bundleID`. The activation notification fires only on
    /// app switches, so a call joined after the switch, or in a browser that
    /// has been in front longer than an offer lives, never reaches it; the
    /// detection tick pulls instead while the microphone is live. Synchronous
    /// so the offer is in the store when the tick reads it. Returns
    /// `suggestionAXUnavailableFlag` when Accessibility is not trusted, which
    /// no read can get past.
    func refreshFrontmost(bundleID: String) -> UInt32 {
        guard AXIsProcessTrusted() else {
            return suggestionAXUnavailableFlag
        }
        guard let application = NSWorkspace.shared.frontmostApplication,
              let frontmostBundleID = application.bundleIdentifier,
              frontmostBundleID.lowercased() == bundleID.lowercased(),
              observedBundleIDs.contains(frontmostBundleID.lowercased())
        else {
            return 0
        }
        evidenceQueue.sync {
            emit(bundleID: frontmostBundleID, processID: application.processIdentifier)
        }
        return 0
    }

    private func emit(bundleID: String, processID: pid_t) {
        guard isActive else {
            return
        }

        let evidence = accessibilityEvidence(for: processID)
        let flags = evidence.axUnavailable ? suggestionAXUnavailableFlag : 0
        bundleID.withCString { bundleIDPointer in
            withOptionalCString(evidence.title) { titlePointer in
                withOptionalCString(evidence.urlHost) { urlHostPointer in
                    callback(
                        callbackContext,
                        bundleIDPointer,
                        titlePointer,
                        urlHostPointer,
                        flags,
                        monotonicNowNs()
                    )
                }
            }
        }
    }
}

@_cdecl("sona_meeting_capture_probe")
public func sonaMeetingCaptureProbe() -> Int32 {
    guard #available(macOS 14.0, *) else {
        return BridgeResult.unsupported.rawValue
    }
    return CGPreflightScreenCaptureAccess()
        ? BridgeResult.ok.rawValue
        : BridgeResult.permissionDenied.rawValue
}

/// The seam a unit check drives: one stream format and one payload through the
/// same two decisions the audio callback makes, without a live SCStream. It
/// returns 0 and fills `out` with `frameCount * channels` interleaved samples,
/// 1 for a format this bridge cannot read, and 2 for a payload that does not
/// match the format it came with.
@_cdecl("sona_meeting_capture_convert_for_test")
public func sonaMeetingCaptureConvertForTest(
    _ sampleRate: Double,
    _ channelsPerFrame: UInt32,
    _ bytesPerFrame: UInt32,
    _ formatFlags: UInt32,
    _ bitsPerChannel: UInt32,
    _ samples: UnsafePointer<Float>,
    _ sampleCount: UInt32,
    _ frameCount: UInt32,
    _ out: UnsafeMutablePointer<Float>
) -> Int32 {
    var description = AudioStreamBasicDescription()
    description.mSampleRate = sampleRate
    description.mFormatID = kAudioFormatLinearPCM
    description.mFormatFlags = formatFlags
    description.mBytesPerPacket = bytesPerFrame
    description.mFramesPerPacket = 1
    description.mBytesPerFrame = bytesPerFrame
    description.mChannelsPerFrame = channelsPerFrame
    description.mBitsPerChannel = bitsPerChannel
    guard let layout = audioPacketLayout(description) else {
        return 1
    }
    let channels = Int(layout.format.channels)
    let planeCount = layout.planar ? channels : 1
    // The planes describe the payload that was actually handed over, not the
    // one the format implies, so a buffer list contradicting its description
    // reaches `interleaveAudio` the way a real one would.
    let planeFloats = Int(sampleCount) / planeCount
    let planes = AudioBufferList.allocate(maximumBuffers: planeCount)
    defer { free(planes.unsafeMutablePointer) }
    for plane in 0..<planeCount {
        planes[plane] = AudioBuffer(
            mNumberChannels: layout.planar ? 1 : UInt32(channels),
            mDataByteSize: UInt32(planeFloats * MemoryLayout<Float>.size),
            mData: UnsafeMutableRawPointer(mutating: samples.advanced(by: plane * planeFloats))
        )
    }
    return interleaveAudio(
        planes: planes,
        frameCount: Int(frameCount),
        channels: channels,
        planar: layout.planar,
        into: out
    ) ? 0 : 2
}

@_cdecl("sona_meeting_capture_start")
public func sonaMeetingCaptureStart(
    _ applicationBundleIDs: UnsafeRawPointer?,
    _ applicationBundleIDCount: UInt,
    _ epoch: UInt64,
    _ sessionHostAnchorNs: UInt64,
    _ packetCallback: PacketCallback?,
    _ statusCallback: StatusCallback?,
    _ callbackContext: UnsafeMutableRawPointer?,
    _ outNativeAnchorValue: UnsafeMutablePointer<Int64>?,
    _ outNativeAnchorTimescale: UnsafeMutablePointer<Int32>?,
    _ outHostMonotonicAnchorNs: UnsafeMutablePointer<UInt64>?,
    _ outSessionOffsetNs: UnsafeMutablePointer<UInt64>?,
    _ outFormatEpoch: UnsafeMutablePointer<UInt64>?,
    _ outHandle: UnsafeMutablePointer<UnsafeMutableRawPointer?>?
) -> Int32 {
    guard #available(macOS 14.0, *),
          let packetCallback,
          let statusCallback,
          let outNativeAnchorValue,
          let outNativeAnchorTimescale,
          let outHostMonotonicAnchorNs,
          let outSessionOffsetNs,
          let outFormatEpoch,
          let outHandle,
          let bundleIDs = boundedBundleIDs(applicationBundleIDs, count: applicationBundleIDCount)
    else {
        return BridgeResult.invalidArgument.rawValue
    }

    outNativeAnchorValue.pointee = 0
    outNativeAnchorTimescale.pointee = 0
    outHostMonotonicAnchorNs.pointee = 0
    outSessionOffsetNs.pointee = 0
    outFormatEpoch.pointee = 0
    outHandle.pointee = nil
    let bridge = CaptureBridge(
        requestedBundleIDs: bundleIDs,
        epoch: epoch,
        packetCallback: packetCallback,
        statusCallback: statusCallback,
        callbackContext: callbackContext
    )
    let (result, clockBridge) = bridge.startSynchronously(sessionHostAnchorNs: sessionHostAnchorNs)
    guard result == BridgeResult.ok.rawValue, let clockBridge else {
        bridge.tearDownStream()
        return result
    }

    outNativeAnchorValue.pointee = clockBridge.nativeAnchor.value
    outNativeAnchorTimescale.pointee = clockBridge.nativeAnchor.timescale
    outHostMonotonicAnchorNs.pointee = clockBridge.hostMonotonicAnchorNs
    outSessionOffsetNs.pointee = clockBridge.sessionOffsetNs
    outFormatEpoch.pointee = clockBridge.formatEpoch
    outHandle.pointee = Unmanaged.passRetained(bridge).toOpaque()
    return BridgeResult.ok.rawValue
}

@_cdecl("sona_meeting_capture_pause")
public func sonaMeetingCapturePause(_ handle: UnsafeMutableRawPointer?) -> Int32 {
    guard let handle else {
        return BridgeResult.invalidArgument.rawValue
    }
    let bridge = Unmanaged<CaptureBridge>.fromOpaque(handle).takeUnretainedValue()
    return bridge.pauseSynchronously()
}

@_cdecl("sona_meeting_capture_resume")
public func sonaMeetingCaptureResume(
    _ handle: UnsafeMutableRawPointer?,
    _ epoch: UInt64,
    _ sessionHostAnchorNs: UInt64,
    _ outNativeAnchorValue: UnsafeMutablePointer<Int64>?,
    _ outNativeAnchorTimescale: UnsafeMutablePointer<Int32>?,
    _ outHostMonotonicAnchorNs: UnsafeMutablePointer<UInt64>?,
    _ outSessionOffsetNs: UnsafeMutablePointer<UInt64>?,
    _ outFormatEpoch: UnsafeMutablePointer<UInt64>?
) -> Int32 {
    guard let handle,
          let outNativeAnchorValue,
          let outNativeAnchorTimescale,
          let outHostMonotonicAnchorNs,
          let outSessionOffsetNs,
          let outFormatEpoch
    else {
        return BridgeResult.invalidArgument.rawValue
    }

    outNativeAnchorValue.pointee = 0
    outNativeAnchorTimescale.pointee = 0
    outHostMonotonicAnchorNs.pointee = 0
    outSessionOffsetNs.pointee = 0
    outFormatEpoch.pointee = 0
    let bridge = Unmanaged<CaptureBridge>.fromOpaque(handle).takeUnretainedValue()
    let (result, clockBridge) = bridge.resumeSynchronously(
        epoch: epoch,
        sessionHostAnchorNs: sessionHostAnchorNs
    )
    guard result == BridgeResult.ok.rawValue, let clockBridge else {
        return result
    }

    outNativeAnchorValue.pointee = clockBridge.nativeAnchor.value
    outNativeAnchorTimescale.pointee = clockBridge.nativeAnchor.timescale
    outHostMonotonicAnchorNs.pointee = clockBridge.hostMonotonicAnchorNs
    outSessionOffsetNs.pointee = clockBridge.sessionOffsetNs
    outFormatEpoch.pointee = clockBridge.formatEpoch
    return BridgeResult.ok.rawValue
}

@_cdecl("sona_meeting_capture_stop")
public func sonaMeetingCaptureStop(_ handle: UnsafeMutableRawPointer?) -> Int32 {
    guard let handle else {
        return BridgeResult.invalidArgument.rawValue
    }
    let bridge = Unmanaged<CaptureBridge>.fromOpaque(handle).takeUnretainedValue()
    return bridge.stopSynchronously()
}

@_cdecl("sona_meeting_capture_abort")
public func sonaMeetingCaptureAbort(_ handle: UnsafeMutableRawPointer?) -> Int32 {
    guard let handle else {
        return BridgeResult.invalidArgument.rawValue
    }
    let bridge = Unmanaged<CaptureBridge>.fromOpaque(handle).takeUnretainedValue()
    return bridge.abortSynchronously()
}

@_cdecl("sona_meeting_capture_destroy")
public func sonaMeetingCaptureDestroy(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else {
        return
    }
    let bridge = Unmanaged<CaptureBridge>.fromOpaque(handle).takeRetainedValue()
    bridge.tearDownStream()
}

@_cdecl("sona_meeting_suggestions_start")
public func sonaMeetingSuggestionsStart(
    _ configuredBundleIDs: UnsafeRawPointer?,
    _ configuredBundleIDCount: UInt,
    _ callback: SuggestionCallback?,
    _ callbackContext: UnsafeMutableRawPointer?,
    _ outHandle: UnsafeMutablePointer<UnsafeMutableRawPointer?>?
) -> Int32 {
    guard let callback,
          let outHandle,
          let bundleIDs = boundedBundleIDs(configuredBundleIDs, count: configuredBundleIDCount)
    else {
        return BridgeResult.invalidArgument.rawValue
    }

    outHandle.pointee = nil
    let observer = SuggestionObserver(
        configuredBundleIDs: bundleIDs,
        callback: callback,
        callbackContext: callbackContext
    )
    observer.start()
    outHandle.pointee = Unmanaged.passRetained(observer).toOpaque()
    return BridgeResult.ok.rawValue
}

@_cdecl("sona_meeting_suggestions_refresh")
public func sonaMeetingSuggestionsRefresh(
    _ handle: UnsafeMutableRawPointer?,
    _ bundleID: UnsafePointer<CChar>?
) -> UInt32 {
    guard let handle, let bundleID else {
        return 0
    }
    let observer = Unmanaged<SuggestionObserver>.fromOpaque(handle).takeUnretainedValue()
    return observer.refreshFrontmost(bundleID: String(cString: bundleID))
}

@_cdecl("sona_meeting_suggestions_stop")
public func sonaMeetingSuggestionsStop(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else {
        return
    }
    let observer = Unmanaged<SuggestionObserver>.fromOpaque(handle).takeRetainedValue()
    observer.stop()
}

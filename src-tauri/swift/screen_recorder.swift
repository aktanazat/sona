import AppKit
import AVFoundation
import CoreGraphics
import CoreImage
import CoreMedia
import CoreVideo
import Darwin
import Foundation
import ScreenCaptureKit

private enum RecorderBridgeResult: Int32 {
    case ok = 0
    case unsupported = 1
    case cameraPermissionDenied = 3
    case microphonePermissionDenied = 4
    case sourceSelectionCancelled = 5
    case sourceUnavailable = 6
    case cameraUnavailable = 7
    case microphoneUnavailable = 8
    case streamFailed = 9
    case timestampDiscontinuity = 10
    case writerFailed = 11
    case outputFinalizeFailed = 12
    case invalidState = 13
}

/// Status-channel codes, deliberately the same numbers as `RecorderBridgeResult` so one table
/// decodes both channels. `droppedVideoFrame` keeps 5, which the bridge table spends on
/// `sourceSelectionCancelled`: cancellation is a call result and never a status, and Rust filters
/// 5 out before it stores a failure.
private enum RecorderStatus: Int32 {
    case streamFailed = 9
    case timestampDiscontinuity = 10
    case writerFailed = 11
    case sourceUnavailable = 6
    case droppedVideoFrame = 5
}

public typealias RecorderStatusCallback = @convention(c) (
    UnsafeMutableRawPointer?,
    Int32,
    UInt64
) -> Void

private struct RecorderDevice: Codable {
    let id: String
    let name: String
}

private struct RecorderPreflight: Codable {
    let availability: String
    let cameraDevices: [RecorderDevice]
    let microphoneDevices: [RecorderDevice]
}
private func availableDevices(for mediaType: AVMediaType) -> [AVCaptureDevice] {
    if mediaType == .video {
        return AVCaptureDevice.DiscoverySession(
            deviceTypes: [.builtInWideAngleCamera, .external],
            mediaType: .video,
            position: .unspecified
        ).devices
    }
    if mediaType == .audio {
        return AVCaptureDevice.DiscoverySession(
            deviceTypes: [.microphone, .external],
            mediaType: .audio,
            position: .unspecified
        ).devices
    }
    return []
}

private final class StartCompletion: @unchecked Sendable {
    private let semaphore = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var resolved = false
    private var result = RecorderBridgeResult.streamFailed.rawValue

    func resolve(_ result: Int32) {
        lock.lock()
        defer { lock.unlock() }
        guard !resolved else { return }
        resolved = true
        self.result = result
        semaphore.signal()
    }

    func wait() -> Int32 {
        semaphore.wait()
        lock.lock()
        defer { lock.unlock() }
        return result
    }

    /// Waits up to `seconds`, returning nil on timeout with the completion still unresolved so
    /// the caller can resolve it through its own path.
    func wait(seconds: Double) -> Int32? {
        guard semaphore.wait(timeout: .now() + seconds) == .success else { return nil }
        lock.lock()
        defer { lock.unlock() }
        return result
    }
}

private final class PreviewPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// How long the recorder waits for the content picker before treating the selection as
/// cancelled. The picker is a process-wide singleton whose callbacks can go silent, and the
/// blocked caller holds the microphone lease, so the wait has to end.
private let pickerTimeoutSeconds: Double = 300

/// Where the camera bubble sits inside a container: a fraction of its width, 16:9, fixed inset.
/// The preview panel and the encoded frame both call this, so the framing the user sees is the
/// framing the file gets. The encode path rounds to even pixels afterwards, for H.264.
private func cameraBubbleRect(in container: CGRect) -> CGRect {
    let width = container.width * 0.20
    let inset: CGFloat = 24
    return CGRect(
        x: container.maxX - width - inset,
        y: inset,
        width: width,
        height: width * 9 / 16
    )
}

private final class PreviewView: NSView {
    let screenLayer = AVSampleBufferDisplayLayer()
    var cameraLayer: AVCaptureVideoPreviewLayer?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
        screenLayer.videoGravity = .resizeAspect
        layer?.addSublayer(screenLayer)
    }

    required init?(coder: NSCoder) {
        nil
    }

    override func layout() {
        super.layout()
        guard let bounds = layer?.bounds else { return }
        screenLayer.frame = bounds
        if let cameraLayer {
            cameraLayer.frame = cameraBubbleRect(in: bounds)
        }
    }
}

@available(macOS 14.0, *)
private final class ScreenRecorder: NSObject, AVCaptureAudioDataOutputSampleBufferDelegate,
    AVCaptureVideoDataOutputSampleBufferDelegate, SCContentSharingPickerObserver, SCStreamDelegate,
    SCStreamOutput
{
    private let cameraID: String?
    private let microphoneID: String?
    private let cameraEnabled: Bool
    private let microphoneEnabled: Bool
    private let statusCallback: RecorderStatusCallback?
    private let callbackContext: UnsafeMutableRawPointer?

    private let controlQueue = DispatchQueue(label: "com.aktanazat.sona.recorder.control")
    private let screenQueue = DispatchQueue(
        label: "com.aktanazat.sona.recorder.screen",
        qos: .userInteractive
    )
    private let cameraQueue = DispatchQueue(
        label: "com.aktanazat.sona.recorder.camera",
        qos: .userInteractive
    )
    private let microphoneQueue = DispatchQueue(
        label: "com.aktanazat.sona.recorder.microphone",
        qos: .userInteractive
    )
    private let encoderQueue = DispatchQueue(
        label: "com.aktanazat.sona.recorder.encoder",
        qos: .userInitiated
    )
    private let controlQueueKey = DispatchSpecificKey<UInt8>()
    private let screenQueueKey = DispatchSpecificKey<UInt8>()
    private let cameraQueueKey = DispatchSpecificKey<UInt8>()
    private let microphoneQueueKey = DispatchSpecificKey<UInt8>()
    private let encoderQueueKey = DispatchSpecificKey<UInt8>()
    private let stateLock = NSLock()
    private let cameraLock = NSLock()
    private let previewLock = NSLock()

    private var pickerCompletion: StartCompletion?
    private var stream: SCStream?
    private var captureSession: AVCaptureSession?
    private var cameraOutput: AVCaptureVideoDataOutput?
    private var microphoneOutput: AVCaptureAudioDataOutput?
    private var previewPanel: PreviewPanel?
    private weak var previewView: PreviewView?
    private var latestPreviewSample: CMSampleBuffer?
    private var previewScheduled = false
    private var latestCamera: (pixelBuffer: CVPixelBuffer, timestamp: CMTime)?

    // stateLock owns this group. Every read and write of these fields takes the lock, including
    // the status-callback gate in report(), which is what makes callback lifetime provable.
    private var callbacksEnabled = false
    private var recordingRequested = false
    private var paused = false
    private var pauseStartedNs: UInt64?
    private var pausedDurationNs: UInt64 = 0
    private var pendingScreen = false
    private var pendingAudio = false
    private var droppedVideoFrames: UInt64 = 0
    private var failure: RecorderBridgeResult?

    // encoderQueue owns this group. controlQueue also writes it in startRecordingSynchronously
    // and in teardown, both of which run after stopInputs has drained the producer and encoder
    // queues, so no encoder work can be in flight against them. pausedDurationNs above is the one
    // field that crosses queues while capture runs, and both sides take stateLock for it.
    private var outputURL: URL?
    private var writer: AVAssetWriter?
    private var videoInput: AVAssetWriterInput?
    private var audioInput: AVAssetWriterInput?
    private var pixelBufferAdaptor: AVAssetWriterInputPixelBufferAdaptor?
    private var firstScreenTimestamp: CMTime?
    private var lastVideoTimestamp: CMTime?
    private var lastAudioTimestamp: CMTime?
    private var outputWidth = 0
    private var outputHeight = 0
    private var ciContext = CIContext(options: [.cacheIntermediates: false])

    init(
        cameraID: String?,
        microphoneID: String?,
        cameraEnabled: Bool,
        microphoneEnabled: Bool,
        statusCallback: RecorderStatusCallback?,
        callbackContext: UnsafeMutableRawPointer?
    ) {
        self.cameraID = cameraID
        self.microphoneID = microphoneID
        self.cameraEnabled = cameraEnabled
        self.microphoneEnabled = microphoneEnabled
        self.statusCallback = statusCallback
        self.callbackContext = callbackContext
        controlQueue.setSpecific(key: controlQueueKey, value: 1)
        screenQueue.setSpecific(key: screenQueueKey, value: 1)
        cameraQueue.setSpecific(key: cameraQueueKey, value: 1)
        microphoneQueue.setSpecific(key: microphoneQueueKey, value: 1)
        encoderQueue.setSpecific(key: encoderQueueKey, value: 1)
    }

    private func synchronouslyOnControlQueue<T>(_ operation: () -> T) -> T {
        if DispatchQueue.getSpecific(key: controlQueueKey) != nil {
            return operation()
        }
        return controlQueue.sync(execute: operation)
    }

    private func drain(_ queue: DispatchQueue, key: DispatchSpecificKey<UInt8>) {
        guard DispatchQueue.getSpecific(key: key) == nil else { return }
        queue.sync {}
    }

    func startPreviewSynchronously() -> Int32 {
        if let permissionFailure = permissionFailure() {
            return permissionFailure.rawValue
        }
        if let deviceFailure = selectedDeviceFailure() {
            return deviceFailure.rawValue
        }

        let completion = StartCompletion()
        controlQueue.async { [weak self] in
            guard let self else {
                completion.resolve(RecorderBridgeResult.streamFailed.rawValue)
                return
            }
            self.stateLock.lock()
            let canStart = self.stream == nil && self.failure == nil
            if canStart {
                self.pickerCompletion = completion
            }
            self.stateLock.unlock()
            guard canStart else {
                completion.resolve(RecorderBridgeResult.invalidState.rawValue)
                return
            }
            DispatchQueue.main.async { [weak self] in
                self?.presentPicker()
            }
        }
        if let result = completion.wait(seconds: pickerTimeoutSeconds) {
            return result
        }
        resolvePicker(.sourceSelectionCancelled)
        return completion.wait()
    }

    func startRecordingSynchronously(at path: String) -> Int32 {
        controlQueue.sync {
            stateLock.lock()
            defer { stateLock.unlock() }
            guard stream != nil, failure == nil, !recordingRequested else {
                return RecorderBridgeResult.invalidState.rawValue
            }
            guard FileManager.default.fileExists(atPath: URL(fileURLWithPath: path).deletingLastPathComponent().path) else {
                return RecorderBridgeResult.writerFailed.rawValue
            }
            outputURL = URL(fileURLWithPath: path)
            clearWriter()
            firstScreenTimestamp = nil
            lastVideoTimestamp = nil
            lastAudioTimestamp = nil
            recordingRequested = true
            paused = false
            pauseStartedNs = nil
            pausedDurationNs = 0
            return RecorderBridgeResult.ok.rawValue
        }
    }

    func pause() -> Int32 {
        controlQueue.sync {
            stateLock.lock()
            defer { stateLock.unlock() }
            guard recordingRequested, !paused, failure == nil else {
                return RecorderBridgeResult.invalidState.rawValue
            }
            paused = true
            pauseStartedNs = DispatchTime.now().uptimeNanoseconds
            return RecorderBridgeResult.ok.rawValue
        }
    }

    func resume() -> Int32 {
        controlQueue.sync {
            stateLock.lock()
            defer { stateLock.unlock() }
            guard recordingRequested, paused, failure == nil else {
                return RecorderBridgeResult.invalidState.rawValue
            }
            if let pauseStartedNs {
                pausedDurationNs &+= DispatchTime.now().uptimeNanoseconds &- pauseStartedNs
            }
            self.pauseStartedNs = nil
            paused = false
            return RecorderBridgeResult.ok.rawValue
        }
    }

    func stopSynchronously() -> (result: Int32, width: Int32, height: Int32, durationMs: UInt64) {
        synchronouslyOnControlQueue {
            // Finalize before honoring a stream error. A stream that stopped on its own, because
            // the display went away or sharing was revoked, still leaves a healthy writer, and the
            // frames it already encoded are the recording the user asked for.
            let stopResult = stopInputs(cancelWriter: false)
            let finished = finishWriting()
            guard finished == .ok else {
                return ((stopResult == .ok ? finished : stopResult).rawValue, 0, 0, 0)
            }

            let durationMs = durationMilliseconds()
            return (
                RecorderBridgeResult.ok.rawValue,
                Int32(outputWidth),
                Int32(outputHeight),
                durationMs
            )
        }
    }

    func cancelSynchronously() {
        synchronouslyOnControlQueue {
            _ = stopInputs(cancelWriter: true)
            removePartialOutput()
        }
    }

    private func presentPicker() {
        stateLock.lock()
        let pending = pickerCompletion != nil
        stateLock.unlock()
        guard pending else { return }
        var configuration = SCContentSharingPickerConfiguration()
        configuration.allowedPickerModes = [.singleWindow, .singleDisplay]
        configuration.allowsChangingSelectedContent = false
        SCContentSharingPicker.shared.defaultConfiguration = configuration
        SCContentSharingPicker.shared.add(self)
        // The system ignores an inactive picker: present() would show nothing and deliver no
        // callback, parking the caller on its semaphore with the microphone lease held.
        SCContentSharingPicker.shared.isActive = true
        SCContentSharingPicker.shared.present()
    }

    func contentSharingPicker(
        _ picker: SCContentSharingPicker,
        didUpdateWith filter: SCContentFilter,
        for _: SCStream?
    ) {
        picker.remove(self)
        startCapture(filter: filter)
    }

    func contentSharingPicker(_ picker: SCContentSharingPicker, didCancelFor _: SCStream?) {
        picker.remove(self)
        resolvePicker(RecorderBridgeResult.sourceSelectionCancelled)
    }

    func contentSharingPickerStartDidFailWithError(_: Error) {
        SCContentSharingPicker.shared.remove(self)
        resolvePicker(RecorderBridgeResult.sourceUnavailable)
    }

    private func startCapture(filter: SCContentFilter) {
        controlQueue.async { [weak self] in
            guard let self else { return }
            let result = self.configureAndStartCapture(filter: filter)
            self.resolvePicker(result)
        }
    }

    private func configureAndStartCapture(filter: SCContentFilter) -> RecorderBridgeResult {
        guard failure == nil else { return .streamFailed }
        let dimensions = outputDimensions(for: filter)
        outputWidth = dimensions.width
        outputHeight = dimensions.height

        let sessionResult = configureCameraSession()
        guard sessionResult == .ok else { return sessionResult }

        let configuration = SCStreamConfiguration()
        configuration.width = dimensions.width
        configuration.height = dimensions.height
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        configuration.queueDepth = 5
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.showsCursor = true
        configuration.capturesAudio = false

        let captureStream = SCStream(filter: filter, configuration: configuration, delegate: self)
        do {
            try captureStream.addStreamOutput(self, type: .screen, sampleHandlerQueue: screenQueue)
        } catch {
            _ = stopInputs(cancelWriter: true)
            return .streamFailed
        }

        stateLock.lock()
        stream = captureStream
        callbacksEnabled = true
        stateLock.unlock()
        let completion = StartCompletion()
        captureStream.startCapture { [weak self] error in
            if error != nil {
                self?.report(.streamFailed)
                completion.resolve(RecorderBridgeResult.streamFailed.rawValue)
                return
            }
            self?.showPreview()
            completion.resolve(RecorderBridgeResult.ok.rawValue)
        }
        let result = completion.wait()
        if result != RecorderBridgeResult.ok.rawValue {
            _ = stopInputs(cancelWriter: true)
            return .streamFailed
        }
        return .ok
    }

    private func resolvePicker(_ result: RecorderBridgeResult) {
        stateLock.lock()
        let completion = pickerCompletion
        pickerCompletion = nil
        stateLock.unlock()
        guard let completion else { return }
        // The picker is process-wide, so give it back as soon as this selection resolves: drop the
        // observer and deactivate, or the next presenter inherits our observer and our sheet.
        DispatchQueue.main.async {
            SCContentSharingPicker.shared.remove(self)
            SCContentSharingPicker.shared.isActive = false
        }
        completion.resolve(result.rawValue)
    }

    private func configureCameraSession() -> RecorderBridgeResult {
        guard cameraEnabled || microphoneEnabled else { return .ok }
        if let deviceFailure = selectedDeviceFailure() { return deviceFailure }

        let session = AVCaptureSession()
        session.beginConfiguration()
        let configured = configureInputs(on: session)
        session.commitConfiguration()
        guard configured == .ok else { return configured }

        captureSession = session
        // Documented order: configure, commit, then start. Starting inside the batch asks
        // AVFoundation to run a session whose inputs have not been applied yet.
        session.startRunning()
        return .ok
    }

    private func configureInputs(on session: AVCaptureSession) -> RecorderBridgeResult {
        if cameraEnabled {
            guard let camera = selectedDevice(for: .video, id: cameraID),
                  let input = try? AVCaptureDeviceInput(device: camera),
                  session.canAddInput(input)
            else {
                return .cameraUnavailable
            }
            session.addInput(input)
            let output = AVCaptureVideoDataOutput()
            output.alwaysDiscardsLateVideoFrames = true
            output.videoSettings = [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
            ]
            output.setSampleBufferDelegate(self, queue: cameraQueue)
            guard session.canAddOutput(output) else { return .cameraUnavailable }
            session.addOutput(output)
            cameraOutput = output
        }

        if microphoneEnabled {
            guard let microphone = selectedDevice(for: .audio, id: microphoneID),
                  let input = try? AVCaptureDeviceInput(device: microphone),
                  session.canAddInput(input)
            else {
                return .microphoneUnavailable
            }
            session.addInput(input)
            let output = AVCaptureAudioDataOutput()
            output.setSampleBufferDelegate(self, queue: microphoneQueue)
            guard session.canAddOutput(output) else { return .microphoneUnavailable }
            session.addOutput(output)
            microphoneOutput = output
        }

        return .ok
    }

    private func selectedDevice(for mediaType: AVMediaType, id: String?) -> AVCaptureDevice? {
        let devices = availableDevices(for: mediaType)
        guard let id else { return devices.first }
        return devices.first { $0.uniqueID == id }
    }

    private func outputDimensions(for filter: SCContentFilter) -> (width: Int, height: Int) {
        let scale = CGFloat(filter.pointPixelScale)
        let sourceWidth = max(2, Int((filter.contentRect.width * scale).rounded(.down)))
        let sourceHeight = max(2, Int((filter.contentRect.height * scale).rounded(.down)))
        let scaleFactor = min(1, min(1920 / Double(sourceWidth), 1080 / Double(sourceHeight)))
        let width = max(2, Int((Double(sourceWidth) * scaleFactor).rounded(.down)) & ~1)
        let height = max(2, Int((Double(sourceHeight) * scaleFactor).rounded(.down)) & ~1)
        return (width, height)
    }

    func stream(_: SCStream, didStopWithError _: Error) {
        screenQueue.async { [weak self] in
            guard let self else { return }
            stateLock.lock()
            let callbacksEnabled = self.callbacksEnabled
            stateLock.unlock()
            guard callbacksEnabled else { return }
            report(.streamFailed)
        }
    }

    func stream(
        _: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .screen, CMSampleBufferIsValid(sampleBuffer) else { return }
        schedulePreview(sampleBuffer)

        stateLock.lock()
        let shouldEncode = callbacksEnabled && recordingRequested && !paused && failure == nil
        if shouldEncode && !pendingScreen {
            pendingScreen = true
            stateLock.unlock()
            encoderQueue.async { [weak self] in
                self?.encodeScreen(sampleBuffer)
            }
            return
        }
        if shouldEncode {
            droppedVideoFrames &+= 1
            let drops = droppedVideoFrames
            stateLock.unlock()
            report(.droppedVideoFrame, dropped: drops)
            return
        }
        stateLock.unlock()
    }

    func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from _: AVCaptureConnection
    ) {
        guard CMSampleBufferIsValid(sampleBuffer) else { return }
        if output === cameraOutput {
            guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
            cameraLock.lock()
            latestCamera = (pixelBuffer, CMSampleBufferGetPresentationTimeStamp(sampleBuffer))
            cameraLock.unlock()
            return
        }

        guard output === microphoneOutput else { return }
        stateLock.lock()
        let shouldEncode = callbacksEnabled && recordingRequested && !paused && failure == nil && !pendingAudio
        if shouldEncode {
            pendingAudio = true
            stateLock.unlock()
            encoderQueue.async { [weak self] in
                self?.encodeAudio(sampleBuffer)
            }
        } else {
            stateLock.unlock()
        }
    }

    private func encodeScreen(_ sampleBuffer: CMSampleBuffer) {
        defer {
            stateLock.lock()
            pendingScreen = false
            stateLock.unlock()
        }
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        guard prepareWriterIfNeeded() else { return }
        guard let timestamp = normalizedTimestamp(CMSampleBufferGetPresentationTimeStamp(sampleBuffer), video: true) else {
            return
        }
        guard let videoInput, let pixelBufferAdaptor, videoInput.isReadyForMoreMediaData else {
            stateLock.lock()
            droppedVideoFrames &+= 1
            let drops = droppedVideoFrames
            stateLock.unlock()
            report(.droppedVideoFrame, dropped: drops)
            return
        }
        var outputBuffer: CVPixelBuffer?
        guard let pool = pixelBufferAdaptor.pixelBufferPool,
              CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &outputBuffer) == kCVReturnSuccess,
              let outputBuffer
        else {
            report(.writerFailed)
            return
        }
        render(screen: pixelBuffer, screenTimestamp: CMSampleBufferGetPresentationTimeStamp(sampleBuffer), to: outputBuffer)
        guard pixelBufferAdaptor.append(outputBuffer, withPresentationTime: timestamp) else {
            report(.writerFailed)
            return
        }
    }

    private func encodeAudio(_ sampleBuffer: CMSampleBuffer) {
        defer {
            stateLock.lock()
            pendingAudio = false
            stateLock.unlock()
        }
        guard let normalized = normalizedTimestamp(CMSampleBufferGetPresentationTimeStamp(sampleBuffer), video: false) else {
            return
        }
        guard let writer, writer.status == .writing, let audioInput else { return }
        guard audioInput.isReadyForMoreMediaData else { return }
        var timing = CMSampleTimingInfo(
            duration: CMSampleBufferGetDuration(sampleBuffer),
            presentationTimeStamp: normalized,
            decodeTimeStamp: .invalid
        )
        var adjusted: CMSampleBuffer?
        guard CMSampleBufferCreateCopyWithNewTiming(
            allocator: kCFAllocatorDefault,
            sampleBuffer: sampleBuffer,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleBufferOut: &adjusted
        ) == noErr,
              let adjusted,
              audioInput.append(adjusted)
        else {
            report(.writerFailed)
            return
        }
    }

    private func prepareWriterIfNeeded() -> Bool {
        if writer != nil { return true }
        guard let outputURL else {
            report(.writerFailed)
            return false
        }
        do {
            let writer = try AVAssetWriter(outputURL: outputURL, fileType: .mp4)
            let videoSettings: [String: Any] = [
                AVVideoCodecKey: AVVideoCodecType.h264,
                AVVideoWidthKey: outputWidth,
                AVVideoHeightKey: outputHeight,
                AVVideoCompressionPropertiesKey: [
                    AVVideoAverageBitRateKey: max(2_000_000, outputWidth * outputHeight * 6),
                    AVVideoExpectedSourceFrameRateKey: 60,
                    AVVideoMaxKeyFrameIntervalKey: 60,
                    AVVideoAllowFrameReorderingKey: false
                ]
            ]
            let videoInput = AVAssetWriterInput(mediaType: .video, outputSettings: videoSettings)
            videoInput.expectsMediaDataInRealTime = true
            guard writer.canAdd(videoInput) else {
                report(.writerFailed)
                return false
            }
            writer.add(videoInput)
            let adaptor = AVAssetWriterInputPixelBufferAdaptor(
                assetWriterInput: videoInput,
                sourcePixelBufferAttributes: [
                    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                    kCVPixelBufferWidthKey as String: outputWidth,
                    kCVPixelBufferHeightKey as String: outputHeight
                ]
            )
            if microphoneEnabled {
                let audioSettings: [String: Any] = [
                    AVFormatIDKey: kAudioFormatMPEG4AAC,
                    AVSampleRateKey: 48_000,
                    AVNumberOfChannelsKey: 1,
                    AVEncoderBitRateKey: 128_000
                ]
                let audioInput = AVAssetWriterInput(mediaType: .audio, outputSettings: audioSettings)
                audioInput.expectsMediaDataInRealTime = true
                guard writer.canAdd(audioInput) else {
                    report(.writerFailed)
                    return false
                }
                writer.add(audioInput)
                self.audioInput = audioInput
            }
            guard writer.startWriting() else {
                report(.writerFailed)
                return false
            }
            writer.startSession(atSourceTime: .zero)
            self.writer = writer
            self.videoInput = videoInput
            self.pixelBufferAdaptor = adaptor
            return true
        } catch {
            report(.writerFailed)
            return false
        }
    }

    private func normalizedTimestamp(_ timestamp: CMTime, video: Bool) -> CMTime? {
        guard timestamp.isValid else {
            report(.timestampDiscontinuity)
            return nil
        }
        if firstScreenTimestamp == nil {
            guard video else { return nil }
            firstScreenTimestamp = timestamp
        }
        guard let firstScreenTimestamp else { return nil }
        stateLock.lock()
        let pausedDurationNs = self.pausedDurationNs
        stateLock.unlock()
        let paused = CMTime(value: Int64(pausedDurationNs), timescale: 1_000_000_000)
        let normalized = CMTimeSubtract(CMTimeSubtract(timestamp, firstScreenTimestamp), paused)
        guard normalized.isValid else {
            report(.timestampDiscontinuity)
            return nil
        }
        if normalized < .zero {
            if video { report(.timestampDiscontinuity) }
            return nil
        }
        if video {
            if let lastVideoTimestamp, CMTimeCompare(normalized, lastVideoTimestamp) <= 0 {
                report(.timestampDiscontinuity)
                return nil
            }
            lastVideoTimestamp = normalized
        } else {
            if let lastAudioTimestamp, CMTimeCompare(normalized, lastAudioTimestamp) <= 0 {
                report(.timestampDiscontinuity)
                return nil
            }
            lastAudioTimestamp = normalized
        }
        return normalized
    }

    private func render(screen: CVPixelBuffer, screenTimestamp: CMTime, to destination: CVPixelBuffer) {
        let outputRect = CGRect(x: 0, y: 0, width: outputWidth, height: outputHeight)
        var image = fitted(CIImage(cvPixelBuffer: screen), in: outputRect)
        if cameraEnabled {
            cameraLock.lock()
            let camera = latestCamera
            cameraLock.unlock()
            if let camera, CMTimeCompare(camera.timestamp, screenTimestamp) <= 0 {
                image = cameraPictureInPicture(CIImage(cvPixelBuffer: camera.pixelBuffer), over: image, outputRect: outputRect)
            }
        }
        ciContext.render(
            image,
            to: destination,
            bounds: outputRect,
            colorSpace: CGColorSpaceCreateDeviceRGB()
        )
    }

    private func fitted(_ image: CIImage, in target: CGRect) -> CIImage {
        let scale = min(target.width / image.extent.width, target.height / image.extent.height)
        let scaled = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let x = target.midX - scaled.extent.midX
        let y = target.midY - scaled.extent.midY
        let black = CIImage(color: .black).cropped(to: target)
        return scaled.transformed(by: CGAffineTransform(translationX: x, y: y)).composited(over: black)
    }

    private func cameraPictureInPicture(_ camera: CIImage, over screen: CIImage, outputRect: CGRect) -> CIImage {
        let slot = cameraBubbleRect(in: outputRect)
        let pipRect = CGRect(
            x: slot.minX,
            y: slot.minY,
            width: max(2, floor(slot.width / 2) * 2),
            height: max(2, floor(slot.height / 2) * 2)
        )
        let scale = max(pipRect.width / camera.extent.width, pipRect.height / camera.extent.height)
        let scaled = camera.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let crop = CGRect(
            x: scaled.extent.midX - pipRect.width / 2,
            y: scaled.extent.midY - pipRect.height / 2,
            width: pipRect.width,
            height: pipRect.height
        )
        let bubble = scaled.cropped(to: crop).transformed(
            by: CGAffineTransform(
                translationX: pipRect.minX - crop.minX,
                y: pipRect.minY - crop.minY
            )
        )
        return bubble.composited(over: screen)
    }

    private func stopInputs(cancelWriter: Bool) -> RecorderBridgeResult {
        stateLock.lock()
        callbacksEnabled = false
        recordingRequested = false
        paused = false
        let stream = self.stream
        self.stream = nil
        let cameraOutput = self.cameraOutput
        let microphoneOutput = self.microphoneOutput
        let captureSession = self.captureSession
        stateLock.unlock()

        if let stream {
            try? stream.removeStreamOutput(self, type: .screen)
        }
        cameraOutput?.setSampleBufferDelegate(nil, queue: nil)
        microphoneOutput?.setSampleBufferDelegate(nil, queue: nil)

        let streamResult = stopStream(stream)
        captureSession?.stopRunning()
        drain(screenQueue, key: screenQueueKey)
        drain(cameraQueue, key: cameraQueueKey)
        drain(microphoneQueue, key: microphoneQueueKey)
        drain(encoderQueue, key: encoderQueueKey)
        stateLock.lock()
        self.cameraOutput = nil
        self.microphoneOutput = nil
        self.captureSession = nil
        stateLock.unlock()
        hidePreview()

        if cancelWriter {
            writer?.cancelWriting()
            clearWriter()
            return streamResult
        }
        return streamResult
    }

    private func stopStream(_ stream: SCStream?) -> RecorderBridgeResult {
        guard let stream else { return .ok }
        let completion = StartCompletion()
        stream.stopCapture { error in
            completion.resolve((error == nil ? RecorderBridgeResult.ok : .streamFailed).rawValue)
        }
        let result = completion.wait()
        return RecorderBridgeResult(rawValue: result) ?? .streamFailed
    }


    private func finishWriting() -> RecorderBridgeResult {
        guard let writer, let videoInput, writer.status == .writing else {
            clearWriter()
            return .outputFinalizeFailed
        }
        videoInput.markAsFinished()
        audioInput?.markAsFinished()
        let completion = StartCompletion()
        writer.finishWriting {
            completion.resolve(
                writer.status == .completed
                    ? RecorderBridgeResult.ok.rawValue
                    : RecorderBridgeResult.outputFinalizeFailed.rawValue
            )
        }
        let result = RecorderBridgeResult(rawValue: completion.wait()) ?? .outputFinalizeFailed
        guard result == .ok,
              let outputURL,
              validatedOutput(at: outputURL)
        else {
            clearWriter()
            return .outputFinalizeFailed
        }
        // Drop the path now that it holds a finalized recording, so a later cancel or destroy can
        // only ever remove a file that was never published.
        self.outputURL = nil
        clearWriter()
        return .ok
    }

    private func validatedOutput(at outputURL: URL) -> Bool {
        // Synchronous accessors on purpose: this runs on controlQueue during stop, and waiting on
        // an unstructured Task would make the user-visible stop path depend on a free
        // cooperative-pool thread. Audio is not required either: a recording stopped inside the
        // first screen-frame interval has no microphone samples yet, and its video still counts.
        let asset = AVURLAsset(url: outputURL)
        let duration = CMTimeGetSeconds(asset.duration)
        return asset.isPlayable
            && !asset.tracks(withMediaType: .video).isEmpty
            && duration.isFinite
            && duration > 0
    }

    private func clearWriter() {
        writer = nil
        videoInput = nil
        audioInput = nil
        pixelBufferAdaptor = nil
    }

    private func removePartialOutput() {
        guard let outputURL else { return }
        try? FileManager.default.removeItem(at: outputURL)
        self.outputURL = nil
    }

    private func durationMilliseconds() -> UInt64 {
        guard let lastVideoTimestamp else { return 0 }
        let seconds = CMTimeGetSeconds(lastVideoTimestamp)
        guard seconds.isFinite, seconds >= 0 else { return 0 }
        return UInt64((seconds * 1_000).rounded(.down))
    }

    /// The capture session under `stateLock`. showPreview runs on the main queue while teardown
    /// clears the field on controlQueue, and an ARC-managed property read against a concurrent
    /// release is a use-after-release, not a torn read.
    private func lockedCaptureSession() -> AVCaptureSession? {
        stateLock.lock()
        defer { stateLock.unlock() }
        return captureSession
    }

    private func showPreview() {
        DispatchQueue.main.async { [weak self] in
            guard let self, self.previewPanel == nil else { return }
            let view = PreviewView(frame: NSRect(x: 0, y: 0, width: 480, height: 270))
            if self.cameraEnabled, let captureSession = self.lockedCaptureSession() {
                let cameraLayer = AVCaptureVideoPreviewLayer(session: captureSession)
                cameraLayer.videoGravity = .resizeAspectFill
                cameraLayer.cornerRadius = 12
                cameraLayer.masksToBounds = true
                view.layer?.addSublayer(cameraLayer)
                view.cameraLayer = cameraLayer
            }
            let panel = PreviewPanel(
                contentRect: NSRect(x: 0, y: 0, width: 480, height: 270),
                styleMask: [.nonactivatingPanel, .titled, .closable],
                backing: .buffered,
                defer: false
            )
            panel.isFloatingPanel = true
            panel.level = .floating
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
            panel.hidesOnDeactivate = false
            panel.title = "Sona Recorder Preview"
            panel.contentView = view
            panel.orderFrontRegardless()
            self.previewPanel = panel
            self.previewView = view
        }
    }

    private func hidePreview() {
        DispatchQueue.main.async { [weak self] in
            self?.previewPanel?.orderOut(nil)
            self?.previewPanel = nil
            self?.previewView = nil
        }
    }

    private func schedulePreview(_ sampleBuffer: CMSampleBuffer) {
        previewLock.lock()
        latestPreviewSample = sampleBuffer
        let schedule = !previewScheduled
        previewScheduled = true
        previewLock.unlock()
        guard schedule else { return }
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.previewLock.lock()
            let sample = self.latestPreviewSample
            self.latestPreviewSample = nil
            self.previewScheduled = false
            self.previewLock.unlock()
            if let sample, let previewView = self.previewView {
                if #available(macOS 15.0, *) {
                    previewView.screenLayer.sampleBufferRenderer.enqueue(sample)
                } else {
                    previewView.screenLayer.enqueue(sample)
                }
            }
        }
    }

    /// The first device permission the recorder needs and does not have, or nil when it has them
    /// all. The screen needs none: the system picker grants the stream exactly what was picked. A
    /// denial can only name a device the caller asked for.
    private func permissionFailure() -> RecorderBridgeResult? {
        if cameraEnabled, AVCaptureDevice.authorizationStatus(for: .video) != .authorized {
            return .cameraPermissionDenied
        }
        if microphoneEnabled, AVCaptureDevice.authorizationStatus(for: .audio) != .authorized {
            return .microphonePermissionDenied
        }
        return nil
    }

    private func selectedDeviceFailure() -> RecorderBridgeResult? {
        if cameraEnabled && selectedDevice(for: .video, id: cameraID) == nil {
            return .cameraUnavailable
        }
        if microphoneEnabled && selectedDevice(for: .audio, id: microphoneID) == nil {
            return .microphoneUnavailable
        }
        return nil
    }

    private func report(_ status: RecorderStatus, dropped: UInt64? = nil) {
        stateLock.lock()
        guard callbacksEnabled else {
            stateLock.unlock()
            return
        }
        statusCallback?(callbackContext, status.rawValue, dropped ?? droppedVideoFrames)
        guard status != .droppedVideoFrame else {
            stateLock.unlock()
            return
        }

        let failure = RecorderBridgeResult(rawValue: status.rawValue) ?? .streamFailed
        let firstFailure = self.failure == nil
        self.failure = failure
        stateLock.unlock()
        guard firstFailure else { return }
        // The writer stays: the core finalizes what was encoded before the failure, and the
        // writer's own status decides whether that file survives.
        controlQueue.async { [weak self] in
            _ = self?.stopInputs(cancelWriter: false)
        }
    }
}

@_cdecl("sona_recorder_preflight_json")
public func sona_recorder_preflight_json() -> UnsafeMutablePointer<CChar>? {
    let supported = ProcessInfo.processInfo.isOperatingSystemAtLeast(
        OperatingSystemVersion(majorVersion: 14, minorVersion: 0, patchVersion: 0)
    )
    let payload = RecorderPreflight(
        availability: supported ? "supported" : "unsupported",
        cameraDevices: availableDevices(for: .video).map {
            RecorderDevice(id: $0.uniqueID, name: $0.localizedName)
        },
        microphoneDevices: availableDevices(for: .audio).map {
            RecorderDevice(id: $0.uniqueID, name: $0.localizedName)
        }
    )
    guard let data = try? JSONEncoder().encode(payload),
          let json = String(data: data, encoding: .utf8)
    else {
        return nil
    }
    return strdup(json)
}

@_cdecl("sona_recorder_free_string")
public func sona_recorder_free_string(_ value: UnsafeMutablePointer<CChar>?) {
    free(value)
}

@_cdecl("sona_recorder_preview_start")
public func sona_recorder_preview_start(
    _ cameraID: UnsafePointer<CChar>?,
    _ microphoneID: UnsafePointer<CChar>?,
    _ cameraEnabled: Int32,
    _ microphoneEnabled: Int32,
    _ statusCallback: RecorderStatusCallback?,
    _ callbackContext: UnsafeMutableRawPointer?,
    _ outHandle: UnsafeMutablePointer<UnsafeMutableRawPointer?>?
) -> Int32 {
    guard ProcessInfo.processInfo.isOperatingSystemAtLeast(
        OperatingSystemVersion(majorVersion: 14, minorVersion: 0, patchVersion: 0)
    ) else {
        return RecorderBridgeResult.unsupported.rawValue
    }
    guard let outHandle else { return RecorderBridgeResult.invalidState.rawValue }
    outHandle.pointee = nil
    let recorder = ScreenRecorder(
        cameraID: cameraID.map(String.init(cString:)),
        microphoneID: microphoneID.map(String.init(cString:)),
        cameraEnabled: cameraEnabled != 0,
        microphoneEnabled: microphoneEnabled != 0,
        statusCallback: statusCallback,
        callbackContext: callbackContext
    )
    let result = recorder.startPreviewSynchronously()
    guard result == RecorderBridgeResult.ok.rawValue else {
        recorder.cancelSynchronously()
        return result
    }
    outHandle.pointee = Unmanaged.passRetained(recorder).toOpaque()
    return RecorderBridgeResult.ok.rawValue
}

@_cdecl("sona_recorder_start")
public func sona_recorder_start(_ handle: UnsafeMutableRawPointer?, _ outputPath: UnsafePointer<CChar>?) -> Int32 {
    guard let handle, let outputPath else { return RecorderBridgeResult.invalidState.rawValue }
    let recorder = Unmanaged<ScreenRecorder>.fromOpaque(handle).takeUnretainedValue()
    return recorder.startRecordingSynchronously(at: String(cString: outputPath))
}

@_cdecl("sona_recorder_pause")
public func sona_recorder_pause(_ handle: UnsafeMutableRawPointer?) -> Int32 {
    guard let handle else { return RecorderBridgeResult.invalidState.rawValue }
    return Unmanaged<ScreenRecorder>.fromOpaque(handle).takeUnretainedValue().pause()
}

@_cdecl("sona_recorder_resume")
public func sona_recorder_resume(_ handle: UnsafeMutableRawPointer?) -> Int32 {
    guard let handle else { return RecorderBridgeResult.invalidState.rawValue }
    return Unmanaged<ScreenRecorder>.fromOpaque(handle).takeUnretainedValue().resume()
}

@_cdecl("sona_recorder_stop")
public func sona_recorder_stop(
    _ handle: UnsafeMutableRawPointer?,
    _ outWidth: UnsafeMutablePointer<Int32>?,
    _ outHeight: UnsafeMutablePointer<Int32>?,
    _ outDurationMs: UnsafeMutablePointer<UInt64>?
) -> Int32 {
    guard let handle else { return RecorderBridgeResult.invalidState.rawValue }
    let result = Unmanaged<ScreenRecorder>.fromOpaque(handle).takeUnretainedValue().stopSynchronously()
    outWidth?.pointee = result.width
    outHeight?.pointee = result.height
    outDurationMs?.pointee = result.durationMs
    return result.result
}

@_cdecl("sona_recorder_cancel")
public func sona_recorder_cancel(_ handle: UnsafeMutableRawPointer?) -> Int32 {
    guard let handle else { return RecorderBridgeResult.invalidState.rawValue }
    Unmanaged<ScreenRecorder>.fromOpaque(handle).takeUnretainedValue().cancelSynchronously()
    return RecorderBridgeResult.ok.rawValue
}

@_cdecl("sona_recorder_destroy")
public func sona_recorder_destroy(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    let recorder = Unmanaged<ScreenRecorder>.fromOpaque(handle).takeRetainedValue()
    recorder.cancelSynchronously()
}

import AVFoundation
import CoreMediaIO
import Foundation
import IOKit.audio
import os

final class SonaCameraProvider: NSObject, CMIOExtensionProviderSource {
    private(set) var provider: CMIOExtensionProvider!
    private var camera: SonaCameraDevice!
    private var clients: Set<UUID> = []

    override init() {
        super.init()
        provider = CMIOExtensionProvider(source: self, clientQueue: DispatchQueue(label: "com.aktanazat.sona.camera.clients"))
        do {
            camera = try SonaCameraDevice(name: "Sona Camera")
            try provider.addDevice(camera.device)
        } catch {
            Logger(subsystem: CameraWatermarkState.extensionID, category: "camera").error("Camera registration failed: \(error.localizedDescription, privacy: .public)")
            exit(EXIT_FAILURE)
        }
    }

    var availableProperties: Set<CMIOExtensionProperty> { [.providerManufacturer] }

    func providerProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionProviderProperties {
        let result = CMIOExtensionProviderProperties(dictionary: [:])
        if properties.contains(.providerManufacturer) { result.manufacturer = "Sona" }
        return result
    }

    func setProviderProperties(_ properties: CMIOExtensionProviderProperties) throws {
        throw readOnlyProperty()
    }

    func connect(to client: CMIOExtensionClient) throws { clients.insert(client.clientID) }

    func disconnect(from client: CMIOExtensionClient) {
        clients.remove(client.clientID)
        if clients.isEmpty { camera.stopAll() }
    }
}

private func readOnlyProperty() -> NSError {
    NSError(domain: NSOSStatusErrorDomain, code: Int(kCMIOHardwareIllegalOperationError),
            userInfo: [NSLocalizedDescriptionKey: "This Sona Camera property is read-only."])
}

final class SonaCameraDevice: NSObject, CMIOExtensionDeviceSource, AVCaptureVideoDataOutputSampleBufferDelegate {
    private(set) var device: CMIOExtensionDevice!
    private var streamSource: SonaCameraStream!
    private let sessionQueue = DispatchQueue(label: "com.aktanazat.sona.camera.capture")
    private let frameQueue = DispatchQueue(label: "com.aktanazat.sona.camera.frames", qos: .userInitiated)
    private var capture: AVCaptureSession?
    private var output: AVCaptureVideoDataOutput?
    private var consumers = 0
    private var timer: DispatchSourceTimer?
    private var selectedCamera: String?
    private var failure: String?
    private var observer: NSObjectProtocol?
    // These fields are confined to frameQueue.
    private var renderer: CameraFrameRenderer?
    private var state = CameraWatermarkState()
    private var lastFrame = CMTime.invalid
    private let logger = Logger(subsystem: CameraWatermarkState.extensionID, category: "camera")

    init(name: String) throws {
        super.init()
        guard let deviceID = UUID(uuidString: CameraWatermarkState.cameraDeviceID),
              let streamID = UUID(uuidString: "810F1027-34C3-407A-B28A-71C81E307BA0") else {
            throw CameraWatermarkError.unsupportedFormat
        }
        device = CMIOExtensionDevice(localizedName: name, deviceID: deviceID, legacyDeviceID: nil, source: self)
        let format = try CameraFrameRenderer.makeFormatDescription()
        let streamFormat = CMIOExtensionStreamFormat(formatDescription: format,
                                                     maxFrameDuration: CameraFrameRenderer.duration,
                                                     minFrameDuration: CameraFrameRenderer.duration,
                                                     validFrameDurations: nil)
        streamSource = SonaCameraStream(id: streamID, format: streamFormat, device: self)
        try device.addStream(streamSource.stream)
    }

    var availableProperties: Set<CMIOExtensionProperty> { [.deviceTransportType, .deviceModel] }

    func deviceProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionDeviceProperties {
        let result = CMIOExtensionDeviceProperties(dictionary: [:])
        if properties.contains(.deviceTransportType) { result.transportType = kIOAudioDeviceTransportTypeVirtual }
        if properties.contains(.deviceModel) { result.model = "Sona Camera" }
        return result
    }

    func setDeviceProperties(_ properties: CMIOExtensionDeviceProperties) throws { throw readOnlyProperty() }

    func start() throws {
        do {
            try sessionQueue.sync {
                if consumers > 0 {
                    guard capture?.isRunning == true else { throw CameraWatermarkError.captureUnavailable }
                    consumers += 1
                    return
                }
                let settings = try CameraWatermarkState.read()
                guard let id = settings.cameraID, !id.isEmpty else { throw CameraWatermarkError.noCamera }
                try startCapture(id: id, settings: settings)
                consumers = 1
                let timer = DispatchSource.makeTimerSource(queue: sessionQueue)
                timer.schedule(deadline: .now(), repeating: 1)
                timer.setEventHandler { [weak self] in self?.refreshState() }
                self.timer = timer
                timer.resume()
            }
        } catch {
            report(error.localizedDescription, streaming: false)
            throw error
        }
    }

    func stop() {
        sessionQueue.async {
            self.consumers = max(0, self.consumers - 1)
            if self.consumers == 0 { self.close() }
        }
    }

    func stopAll() {
        sessionQueue.async { self.consumers = 0; self.close() }
    }

    private func close() {
        timer?.cancel()
        timer = nil
        stopCapture()
        selectedCamera = nil
        report("Sona Camera is not streaming.", streaming: false)
    }

    private func startCapture(id: String, settings: CameraWatermarkState) throws {
        guard id != CameraWatermarkState.cameraDeviceID,
              let physical = AVCaptureDevice.DiscoverySession(deviceTypes: [.builtInWideAngleCamera, .external],
                                                              mediaType: .video, position: .unspecified)
                .devices.first(where: { $0.uniqueID == id && $0.localizedName != "Sona Camera" }) else {
            throw CameraWatermarkError.noCamera
        }
        guard AVCaptureDevice.authorizationStatus(for: .video) == .authorized else {
            throw CameraWatermarkError.permissionRequired
        }
        let input = try AVCaptureDeviceInput(device: physical)
        let session = AVCaptureSession()
        let output = AVCaptureVideoDataOutput()
        output.alwaysDiscardsLateVideoFrames = true
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
        guard session.canAddInput(input), session.canAddOutput(output) else { throw CameraWatermarkError.captureUnavailable }
        session.beginConfiguration()
        if session.canSetSessionPreset(.hd1280x720) { session.sessionPreset = .hd1280x720 }
        session.addInput(input)
        session.addOutput(output)
        session.commitConfiguration()
        let frames = try CameraFrameRenderer()
        frameQueue.sync { self.renderer = frames; self.state = settings; self.lastFrame = .invalid }
        output.setSampleBufferDelegate(self, queue: frameQueue)
        self.capture = session
        self.output = output
        selectedCamera = id
        observer = NotificationCenter.default.addObserver(forName: AVCaptureSession.runtimeErrorNotification,
                                                         object: session, queue: nil) { [weak self] event in
            let message = (event.userInfo?[AVCaptureSessionErrorKey] as? Error)?.localizedDescription
                ?? CameraWatermarkError.captureUnavailable.localizedDescription
            self?.sessionQueue.async { [weak self] in
                guard let self else { return }
                self.failure = message
                self.stopCapture()
                self.report(message, streaming: false)
            }
        }
        session.startRunning()
        guard session.isRunning else {
            stopCapture()
            throw CameraWatermarkError.captureUnavailable
        }
        failure = nil
        report("Sona Camera is streaming your selected camera.", streaming: true)
    }

    private func stopCapture() {
        if let observer { NotificationCenter.default.removeObserver(observer); self.observer = nil }
        output?.setSampleBufferDelegate(nil, queue: nil)
        capture?.stopRunning()
        capture = nil
        output = nil
        frameQueue.sync { self.renderer = nil; self.lastFrame = .invalid; self.state.recording = false }
    }

    private func refreshState() {
        guard consumers > 0 else { return }
        do {
            let settings = try CameraWatermarkState.read()
            if settings.cameraID != selectedCamera {
                stopCapture()
                selectedCamera = settings.cameraID
                guard let id = settings.cameraID else { throw CameraWatermarkError.noCamera }
                try startCapture(id: id, settings: settings)
            }
            frameQueue.async { self.state = settings }
            let marked = settings.showsNotice(at: Date().timeIntervalSince1970)
            report(failure ?? (marked ? "Your video shows “Recording with Sona”." : "Your normal video is passing through without a recording label."),
                   streaming: capture?.isRunning == true)
        } catch {
            // An unreadable recording state must never leave a stale label.
            frameQueue.async { self.state.recording = false }
            failure = error.localizedDescription
            report(error.localizedDescription, streaming: capture?.isRunning == true)
        }
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let renderer, let image = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let now = CMClockGetTime(CMClockGetHostTimeClock())
        if lastFrame.isValid && CMTimeCompare(CMTimeSubtract(now, lastFrame), CameraFrameRenderer.duration) < 0 { return }
        do {
            guard let sample = try renderer.render(image, at: now, watermarked: state.showsNotice(at: Date().timeIntervalSince1970)) else { return }
            lastFrame = lastFrame.isValid
                ? CMTimeMaximum(CMTimeAdd(lastFrame, CameraFrameRenderer.duration),
                                CMTimeSubtract(now, CameraFrameRenderer.duration))
                : now
            let nanoseconds = CMTimeConvertScale(now, timescale: 1_000_000_000, method: .default).value
            guard nanoseconds >= 0 else { return }
            streamSource.stream.send(sample, discontinuity: [], hostTimeInNanoseconds: UInt64(nanoseconds))
        } catch {
            let message = error.localizedDescription
            sessionQueue.async { self.failure = message; self.report(message, streaming: false) }
        }
    }

    private func report(_ message: String, streaming: Bool) {
        do {
            try CameraWatermarkStatus(message: message, streaming: streaming, updatedAt: Date().timeIntervalSince1970).write()
        } catch { logger.error("Camera status unavailable: \(error.localizedDescription, privacy: .public)") }
    }
}

final class SonaCameraStream: NSObject, CMIOExtensionStreamSource {
    private(set) var stream: CMIOExtensionStream!
    private let format: CMIOExtensionStreamFormat
    private unowned let device: SonaCameraDevice

    init(id: UUID, format: CMIOExtensionStreamFormat, device: SonaCameraDevice) {
        self.format = format
        self.device = device
        super.init()
        stream = CMIOExtensionStream(localizedName: "Sona Camera video", streamID: id, direction: .source, clockType: .hostTime, source: self)
    }

    var formats: [CMIOExtensionStreamFormat] { [format] }
    var availableProperties: Set<CMIOExtensionProperty> { [.streamActiveFormatIndex, .streamFrameDuration] }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionStreamProperties {
        let result = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) { result.activeFormatIndex = 0 }
        if properties.contains(.streamFrameDuration) { result.frameDuration = CameraFrameRenderer.duration }
        return result
    }

    func setStreamProperties(_ properties: CMIOExtensionStreamProperties) throws {
        if let index = properties.activeFormatIndex, index != 0 { throw CameraWatermarkError.unsupportedFormat }
        if let duration = properties.frameDuration, CMTimeCompare(duration, CameraFrameRenderer.duration) != 0 {
            throw CameraWatermarkError.unsupportedFormat
        }
    }

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        // macOS owns the consuming app's Camera privacy permission. Sona owns
        // the explicit physical-camera selection; no implicit default is used.
        (try? CameraWatermarkState.read().cameraID) != nil
    }

    func startStream() throws { try device.start() }
    func stopStream() throws { device.stop() }
}

import AppKit
import AVFoundation
import Observation
import SystemExtensions

@Observable @MainActor
final class CameraWatermarkController: NSObject, OSSystemExtensionRequestDelegate {
    static let shared = CameraWatermarkController()

    struct CameraChoice: Identifiable {
        let id: String
        let name: String
    }

    private(set) var enabled = false
    private(set) var cameraID = ""
    private(set) var cameras: [CameraChoice] = []
    private(set) var installed = false
    private(set) var busy = false
    private(set) var status = "The optional camera is not installed."
    private(set) var error: String?
    private(set) var cameraAccess = AVCaptureDevice.authorizationStatus(for: .video)
    private(set) var streamStatus: String?
    @ObservationIgnored private var core: Core?
    @ObservationIgnored private var recording = false
    @ObservationIgnored private var heartbeat: Task<Void, Never>?
    @ObservationIgnored private var requests: [OSSystemExtensionRequest] = []

    @ObservationIgnored private var changingRequests: Set<ObjectIdentifier> = []
    private override init() {
        super.init()
        do {
            let saved = try CameraWatermarkState.read()
            enabled = saved.watermarkEnabled
            cameraID = saved.cameraID ?? ""
        } catch let failure as CocoaError where failure.code == .fileReadNoSuchFile {
            // A new installation remains off, with no camera selected.
        } catch {
            self.error = error.localizedDescription
        }
        // A stale recording flag must not survive a shell restart.
        publish()
    }

    func attach(core: Core) { self.core = core }

    func setRecording(_ recording: Bool) {
        self.recording = recording
        publish()
        updateHeartbeat()
    }

    func setEnabled(_ value: Bool) {
        enabled = value
        error = nil
        publish()
        updateHeartbeat()
    }

    func selectCamera(_ id: String) {
        guard id.isEmpty || cameras.contains(where: { $0.id == id }) else { return }
        cameraID = id
        error = nil
        publish()
    }

    func refresh() {
        cameraAccess = AVCaptureDevice.authorizationStatus(for: .video)
        // Discovery enumerates devices. It never creates an input or starts a session.
        cameras = AVCaptureDevice.DiscoverySession(
            deviceTypes: [.builtInWideAngleCamera, .external], mediaType: .video, position: .unspecified
        ).devices.filter { $0.uniqueID != CameraWatermarkState.cameraDeviceID && $0.localizedName != "Sona Camera" }
            .map { CameraChoice(id: $0.uniqueID, name: $0.localizedName) }
        if let report = try? CameraWatermarkStatus.read(),
           Date().timeIntervalSince1970 - report.updatedAt < CameraWatermarkState.lifetime {
            streamStatus = report.message
        } else { streamStatus = nil }
        guard !busy else { return }
        submit(OSSystemExtensionRequest.propertiesRequest(
            forExtensionWithIdentifier: CameraWatermarkState.extensionID, queue: .main))
    }

    func requestCameraAccess() {
        error = nil
        guard cameraAccess == .notDetermined else { openCameraPrivacy(); return }
        AVCaptureDevice.requestAccess(for: .video) { _ in
            Task { @MainActor in self.refresh() }
        }
    }

    func install() {
        guard !busy else { return }
        guard Bundle.main.bundleURL.path.hasPrefix("/Applications/") else {
            error = "Move Sona to Applications before installing its camera."
            return
        }
        let bundled = Bundle.main.bundleURL.appendingPathComponent(
            "Contents/Library/SystemExtensions/\(CameraWatermarkState.extensionID).systemextension")
        guard FileManager.default.fileExists(atPath: bundled.path) else {
            error = "This Sona app does not contain the signed camera extension. Reinstall the complete app."
            return
        }
        error = nil
        busy = true
        status = "Asking macOS to install Sona Camera…"
        submit(OSSystemExtensionRequest.activationRequest(
            forExtensionWithIdentifier: CameraWatermarkState.extensionID, queue: .main), change: true)
    }

    func uninstall() {
        guard !busy else { return }
        enabled = false
        publish()
        updateHeartbeat()
        busy = true
        error = nil
        status = "Asking macOS to remove Sona Camera…"
        submit(OSSystemExtensionRequest.deactivationRequest(
            forExtensionWithIdentifier: CameraWatermarkState.extensionID, queue: .main), change: true)
    }

    func openCameraPrivacy() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera") {
            NSWorkspace.shared.open(url)
        }
    }

    func openExtensionSettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.ExtensionsPreferences") {
            NSWorkspace.shared.open(url)
        }
    }

    private func publish() {
        let state = CameraWatermarkState(watermarkEnabled: enabled, recording: recording,
                                        cameraID: cameraID.isEmpty ? nil : cameraID,
                                        updatedAt: Date().timeIntervalSince1970)
        do { try state.write() }
        catch { self.error = error.localizedDescription }
    }

    private func updateHeartbeat() {
        guard enabled && recording else {
            heartbeat?.cancel()
            heartbeat = nil
            return
        }
        guard heartbeat == nil, core != nil else { return }
        heartbeat = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, let core = self.core else { return }
                do {
                    let active: MeetingConsentPanelSessionState? = try await core.request("meeting_consent_panel_active_state")
                    guard !Task.isCancelled else { return }
                    self.recording = active?.snapshot.phase == .capturingRecording
                    self.publish()
                    guard self.recording else { self.heartbeat = nil; return }
                } catch {
                    self.recording = false
                    self.publish()
                    self.heartbeat = nil
                    return
                }
                do { try await Task.sleep(for: .seconds(2)) }
                catch { return }
            }
        }
    }
    private func submit(_ request: OSSystemExtensionRequest, change: Bool = false) {
        request.delegate = self
        if change { changingRequests.insert(ObjectIdentifier(request)) }
        requests.append(request)
        OSSystemExtensionManager.shared.submitRequest(request)
    }

    nonisolated func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        Task { @MainActor in
            self.status = "Waiting for your approval in System Settings → General → Login Items & Extensions → Camera Extensions."
            self.busy = false
        }
    }

    nonisolated func request(_ request: OSSystemExtensionRequest,
                             actionForReplacingExtension existing: OSSystemExtensionProperties,
                             withExtension replacement: OSSystemExtensionProperties) -> OSSystemExtensionRequest.ReplacementAction {
        existing.bundleIdentifier == CameraWatermarkState.extensionID
            && replacement.bundleIdentifier == CameraWatermarkState.extensionID ? .replace : .cancel
    }

    nonisolated func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {
        Task { @MainActor in
            self.requests.removeAll { $0 === request }
            guard self.changingRequests.remove(ObjectIdentifier(request)) != nil else { return }
            self.busy = false
            self.status = result == .willCompleteAfterReboot ? "Restart this Mac to finish the camera change." : "The camera request finished. Checking availability…"
            if result != .willCompleteAfterReboot { self.refresh() }
        }
    }

    nonisolated func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        let message = error.localizedDescription
        Task { @MainActor in
            self.requests.removeAll { $0 === request }
            self.busy = false
            self.error = message
            self.changingRequests.remove(ObjectIdentifier(request))
            self.status = "The camera change did not finish."
        }
    }

    nonisolated func request(_ request: OSSystemExtensionRequest, foundProperties properties: [OSSystemExtensionProperties]) {
        let enabled = properties.contains { $0.isEnabled && !$0.isUninstalling }
        let waiting = properties.contains { $0.isAwaitingUserApproval }
        Task { @MainActor in
            self.requests.removeAll { $0 === request }
            self.installed = enabled
            self.status = enabled ? "Sona Camera is available. Choose it in your call app's camera menu."
                : waiting ? "Waiting for your approval in Camera Extensions in System Settings."
                : "Sona Camera is not installed."
        }
    }
}

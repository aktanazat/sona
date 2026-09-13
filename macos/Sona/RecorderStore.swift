import AVFoundation
import AppKit
import CoreGraphics
import Foundation
import Observation

extension CoreEvent {
    /// `RecorderStateChangedEvent::NAME`: every phase change the core makes,
    /// plus one heartbeat a second while a recording runs.
    static let recorderStateChanged = "recorder-state-changed-event"
}

/// The `request` object `recorder_preview_start` takes.
private struct RecorderPreviewStartParams: Encodable {
    let request: RecorderStartRequest
}

/// The screen recorder, ported from src/components/recorder/RecorderDialog.tsx.
///
/// The core owns the recording: it raises the native source picker, holds the
/// microphone lease, writes the file and publishes a snapshot for every phase
/// change. This store keeps the dialog's own machine
/// (src/components/recorder/recorderMachine.ts) over those snapshots, asks the
/// three OS permissions the React build asked a Tauri plugin for, and carries
/// the clock forward between the core's one-per-second heartbeats.
@MainActor
@Observable
final class RecorderStore {
    /// The machine's state: the last snapshot, the preflight, which permission
    /// is missing and whether the user has been asked for it yet.
    private(set) var state = RecorderUiState()
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    /// Why Finder or Launch Services refused the saved file, shown while
    /// `state.revealFailed` stands.
    private(set) var revealNote: String?

    /// The inputs, as the dialog's `useState` held them: the microphone is on
    /// by default, the camera is not.
    var cameraEnabled = false
    var microphoneEnabled = true
    var cameraDeviceId: String?
    var microphoneDeviceId: String?

    /// A pick is in flight, so the button that starts one is dead.
    private(set) var selectionPending = false
    /// An OS permission prompt or re-check is in flight.
    private(set) var permissionPending = false
    /// The local clock, moved four times a second while recording.
    private(set) var tick: Date = .now

    @ObservationIgnored private let core: Core
    /// Bumped on every open and close: a reply that lands after the sheet has
    /// gone must not touch the next session's state.
    @ObservationIgnored private var session = 0
    /// The last snapshot the core itself published. A command error says
    /// nothing new when this already names the failure.
    @ObservationIgnored private var latestNativeSnapshot: RecorderSnapshot?
    /// When the elapsed value in `state.snapshot` was measured.
    @ObservationIgnored private var snapshotAt: Date = .now
    @ObservationIgnored private var ticker: Task<Void, Never>?

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.recorderStateChanged) { [weak self] line in self?.receive(line) }
    }

    // MARK: - What the sheet reads

    var snapshot: RecorderSnapshot { state.snapshot }
    var phase: RecorderPhase { state.snapshot.phase }
    var failure: RecorderFailureCode? { state.snapshot.failure }
    /// The permission the notice, the grant button and the Settings pane name.
    var permission: RecorderPermission { state.permission ?? .screen }
    var cameraDevices: [RecorderDevice] { state.preflight?.cameraDevices ?? [] }
    var microphoneDevices: [RecorderDevice] { state.preflight?.microphoneDevices ?? [] }
    /// `canCloseRecorder`: the phases that may be dismissed.
    var canClose: Bool { state.snapshot.phase.closable }
    /// `recorderHasCapture`: whether the clock means anything yet.
    var hasCapture: Bool { state.snapshot.hasCapture }
    /// The frames the writer could not keep up with, as the core counts them.
    var droppedFrames: UInt64 { state.snapshot.droppedVideoFrames }
    /// What the dialog offers after a failure.
    var recovery: RecorderRecovery { state.snapshot.failure?.recovery ?? .done }
    /// The permission a failure names, when it names one.
    var recoveryPermission: RecorderPermission? {
        RecorderPermission(failure: state.snapshot.failure)
    }

    /// The elapsed time: the core's `elapsedMs`, carried forward locally while
    /// recording so the clock does not stutter between heartbeats. A pause
    /// keeps the native value fixed, which is what the core measures too.
    var elapsed: TimeInterval {
        let base = Double(state.snapshot.elapsedMs) / 1000
        guard state.snapshot.phase == .recording else { return base }
        return base + max(0, tick.timeIntervalSince(snapshotAt))
    }

    /// The device the summary names, when one is on and known.
    var cameraName: String? {
        guard cameraEnabled else { return nil }
        return cameraDevices.first { $0.id == cameraDeviceId }?.name
    }

    var microphoneName: String? {
        guard microphoneEnabled else { return nil }
        return microphoneDevices.first { $0.id == microphoneDeviceId }?.name
    }

    // MARK: - Opening and closing

    /// The first load, and every later open: forget the last session and ask
    /// the core what is possible.
    func start() async {
        session += 1
        latestNativeSnapshot = nil
        revealNote = nil
        selectionPending = false
        permissionPending = false
        dispatch(.reset)
        await loadPreflight()
    }

    /// The dialog's unmount. A preview nobody is watching is cancelled, the
    /// way closing the React dialog cancelled it, and the session moves on so
    /// a late reply cannot land on the next open.
    func stop() {
        let previewing = state.snapshot.phase == .previewing
        session += 1
        selectionPending = false
        permissionPending = false
        if previewing { cancelPreview() }
    }

    /// Whether the sheet may close now. `previewing` is cancelled by `stop`,
    /// which runs however the sheet went away.
    func requestClose() -> Bool { canClose }

    // MARK: - Commands

    /// `recorder_preflight`: is capture supported, is another Sona capture
    /// holding the microphone, and which cameras and microphones exist.
    func loadPreflight() async {
        let session = session
        latestNativeSnapshot = nil
        dispatch(.checking)
        do {
            let preflight: RecorderPreflight = try await core.request("recorder_preflight")
            guard session == self.session else { return }
            adopt(preflight)
            dispatch(.preflight(preflight))
            error = nil
        } catch {
            guard session == self.session else { return }
            self.error = error.localizedDescription
            dispatch(.failure(.streamFailed))
        }
    }

    /// The one button that reaches the native picker: check the permissions the
    /// chosen inputs need first, then let the core raise the picker.
    func chooseScreen() {
        guard !selectionPending else { return }
        if let missing = firstMissingPermission() {
            dispatch(.permission(missing, requested: false))
            return
        }
        let session = session
        selectionPending = true
        Task {
            await startPreview()
            if session == self.session { selectionPending = false }
        }
    }

    /// `recorder_start`: the preview becomes a recording on disk.
    func startRecording() {
        Task { await run("recorder_start", phase: .starting) }
    }

    /// `recorder_pause`.
    func pause() {
        Task { await run("recorder_pause", phase: .paused) }
    }

    /// `recorder_resume`.
    func resume() {
        Task { await run("recorder_resume", phase: .recording) }
    }

    /// `recorder_stop`: finalize the file and publish where it landed.
    func stopAndSave() {
        Task { await run("recorder_stop", phase: .finalizing) }
    }

    /// `recorder_preview_stop`: drop the chosen source and go back to setup.
    func stopPreview() {
        Task { await run("recorder_preview_stop", phase: .idle) }
    }

    /// `recorder_cancel`: give up whatever is in flight, in any phase that
    /// holds work.
    func cancelPreview() {
        Task { await run("recorder_cancel", phase: .idle) }
    }

    /// The failure footer's "Change": clear the failure and show setup again.
    func clearFailure() {
        dispatch(.clearFailure)
    }

    /// The failure footer's "Re-check": name the permission the failure named
    /// and offer the re-check path for it.
    func recheckFailedPermission() {
        guard let permission = recoveryPermission else { return }
        dispatch(.permission(permission, requested: true))
    }

    /// The failure footer's "Retry": ask the core everything again.
    func retry() {
        Task { await loadPreflight() }
    }

    // MARK: - Permissions

    /// One flow for both buttons. Granting asks the OS first; re-checking only
    /// re-reads the answer the user gave in System Settings. A granted
    /// permission goes straight on to the picker.
    func resolvePermission(request: Bool) {
        guard !permissionPending else { return }
        let permission = permission
        let session = session
        permissionPending = true
        Task {
            if request { await permission.requestAccess() }
            let granted = permission.granted
            guard session == self.session else { return }
            permissionPending = false
            guard granted else {
                dispatch(.permission(permission, requested: true))
                return
            }
            chooseScreen()
        }
    }

    /// The Privacy pane that grants the missing permission.
    func openPermissionSettings() {
        guard let url = permission.settingsURL else { return }
        NSWorkspace.shared.open(url)
    }

    // MARK: - The saved file

    /// Finder, with the recording selected.
    func revealRecording() {
        guard let url = outputURL else {
            return failReveal("The recording is no longer where Sona saved it.")
        }
        NSWorkspace.shared.activateFileViewerSelecting([url])
        revealNote = nil
    }

    /// The recording, in whatever plays it.
    func openRecording() {
        guard let url = outputURL else {
            return failReveal("The recording is no longer where Sona saved it.")
        }
        guard NSWorkspace.shared.open(url) else {
            return failReveal("macOS could not open the recording.")
        }
        revealNote = nil
    }

    private var outputURL: URL? {
        guard let path = state.snapshot.outputPath else { return nil }
        let url = URL(fileURLWithPath: path)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    private func failReveal(_ note: String) {
        revealNote = note
        dispatch(.revealFailed)
    }

    // MARK: - The core's side

    /// `recorder_preview_start`: the core raises the native picker with these
    /// inputs and answers with the preview, a cancellation, or a failure.
    private func startPreview() async {
        let session = session
        let request = RecorderStartRequest(
            cameraEnabled: cameraEnabled,
            cameraDeviceId: cameraEnabled ? cameraDeviceId : nil,
            microphoneEnabled: microphoneEnabled,
            microphoneDeviceId: microphoneEnabled ? microphoneDeviceId : nil)
        latestNativeSnapshot = nil
        dispatch(.phase(.selectingSource))
        do {
            let snapshot: RecorderSnapshot = try await core.request(
                "recorder_preview_start", RecorderPreviewStartParams(request: request))
            guard session == self.session else { return }
            applyNative(snapshot)
            error = nil
        } catch {
            guard session == self.session else { return }
            report(error)
        }
    }

    /// Every other recorder command: show the phase the command is reaching
    /// for, then let its snapshot or its error decide what really happened.
    private func run(_ method: String, phase next: RecorderPhase) async {
        let session = session
        latestNativeSnapshot = nil
        dispatch(.phase(next))
        do {
            let snapshot: RecorderSnapshot = try await core.request(method)
            guard session == self.session else { return }
            applyNative(snapshot)
            error = nil
        } catch {
            guard session == self.session else { return }
            report(error)
        }
    }

    /// A refused command. `invalid_state` is the recorder's own error type; the
    /// machine invents `streamFailed` only when no native snapshot has already
    /// said what went wrong.
    private func report(_ failure: Error) {
        if let refusal = (failure as? CoreError)?.remote(as: RecorderCommandError.self) {
            switch refusal {
            case .invalidState: error = "The recorder cannot do that from its current state."
            }
        } else {
            error = failure.localizedDescription
        }
        if let fallback = RecorderSnapshot.commandFailureFallback(latest: latestNativeSnapshot) {
            dispatch(.failure(fallback))
        }
    }

    private func receive(_ line: Data) {
        do {
            let event: RecorderStateChangedEvent = try Core.payload(line)
            applyNative(event.snapshot)
        } catch {
            self.error = "The recorder sent a state the shell could not read."
        }
    }

    private func applyNative(_ snapshot: RecorderSnapshot) {
        latestNativeSnapshot = snapshot
        dispatch(.snapshot(snapshot))
    }

    /// Keep a device choice the new preflight still offers, fall back to the
    /// first one it does, and switch an input off when nothing can serve it.
    private func adopt(_ preflight: RecorderPreflight) {
        if !preflight.cameraDevices.contains(where: { $0.id == cameraDeviceId }) {
            cameraDeviceId = preflight.cameraDevices.first?.id
        }
        if !preflight.microphoneDevices.contains(where: { $0.id == microphoneDeviceId }) {
            microphoneDeviceId = preflight.microphoneDevices.first?.id
        }
        if preflight.cameraDevices.isEmpty { cameraEnabled = false }
        if preflight.microphoneDevices.isEmpty { microphoneEnabled = false }
    }

    /// The machine's only door. Re-bases the local clock whenever the phase or
    /// the elapsed value moved, and keeps the tick running only while recording.
    private func dispatch(_ action: RecorderAction) {
        let previous = state.snapshot
        state = state.applying(action)
        if !state.revealFailed { revealNote = nil }
        if state.snapshot.phase != previous.phase || state.snapshot.elapsedMs != previous.elapsedMs {
            snapshotAt = .now
            tick = .now
        }
        syncTicker()
    }

    private func syncTicker() {
        guard state.snapshot.phase == .recording else {
            ticker?.cancel()
            ticker = nil
            return
        }
        guard ticker == nil else { return }
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 250_000_000)
                guard let self, !Task.isCancelled else { return }
                tick = .now
            }
        }
    }

    /// `requiredPermissions`: the screen always, and each input the user asked
    /// for that has a device behind it.
    private func requiredPermissions() -> [RecorderPermission] {
        var permissions: [RecorderPermission] = [.screen]
        if cameraEnabled, !cameraDevices.isEmpty { permissions.append(.camera) }
        if microphoneEnabled, !microphoneDevices.isEmpty { permissions.append(.microphone) }
        return permissions
    }

    private func firstMissingPermission() -> RecorderPermission? {
        requiredPermissions().first { !$0.granted }
    }
}

extension RecorderPermission {
    /// `checkScreenRecordingPermission` and its two siblings. The React build
    /// reached these through tauri-plugin-macos-permissions; the shell asks
    /// macOS itself.
    var granted: Bool {
        switch self {
        case .screen: CGPreflightScreenCaptureAccess()
        case .camera: AVCaptureDevice.authorizationStatus(for: .video) == .authorized
        case .microphone: AVCaptureDevice.authorizationStatus(for: .audio) == .authorized
        }
    }

    /// The OS prompt. The two device accesses wait for the user's answer;
    /// screen recording answers at once and sends the user to Settings, which
    /// is why the dialog always re-checks afterwards.
    func requestAccess() async {
        switch self {
        case .screen:
            _ = await Task.detached { CGRequestScreenCaptureAccess() }.value
        case .camera:
            _ = await AVCaptureDevice.requestAccess(for: .video)
        case .microphone:
            _ = await AVCaptureDevice.requestAccess(for: .audio)
        }
    }

    /// The Privacy pane that grants it, as the React dialog opened it.
    var settingsURL: URL? {
        switch self {
        case .screen:
            URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        case .camera:
            URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera")
        case .microphone:
            URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        }
    }
}

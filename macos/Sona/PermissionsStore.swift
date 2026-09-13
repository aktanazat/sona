import AVFoundation
import AppKit
import Foundation

/// The two permissions Sona cannot work without, and the one place that knows
/// which process each of them belongs to.
///
/// Microphone is checked natively, because the answer is about a process and
/// the shell can read its own. Accessibility is asked of the core, because the
/// core is the process that types: `AXIsProcessTrusted` answers only for its
/// caller, so a native check here would report on a window that never sends a
/// keystroke. `get_context_diagnostics` runs that check inside the core
/// (src-tauri/src/context/mod.rs:826) and is non-prompting, which is also what
/// makes it safe to poll.
///
/// Neither permission is requested on Sona's behalf by anything else: the grant
/// happens in System Settings or in the system's consent dialog, so this store
/// polls while it waits, with a failure budget, exactly as the web onboarding
/// did.
@MainActor
@Observable
final class PermissionsStore {
    /// Accessibility for the process that types, as the core reports it.
    private(set) var accessibility: PermissionState = .checking
    /// The microphone authorization of this process.
    private(set) var microphone: PermissionState = .checking
    /// Whether this window's own process is trusted. Not a gate — it is the
    /// reason the accessibility row can say which name to look for.
    private(set) var shellTrusted = false
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    /// Called once, the first time both permissions are held.
    @ObservationIgnored var onAllGranted: () -> Void = {}

    @ObservationIgnored private let core: Core
    @ObservationIgnored private var poll: Task<Void, Never>?
    @ObservationIgnored private var failures = 0
    @ObservationIgnored private var inputStarted = false
    @ObservationIgnored private var announced = false

    private static let pollInterval = Duration.seconds(1)
    /// After this many consecutive failed checks the poll stops for good, and
    /// the rows fall back to their own "Re-check".
    private static let failureBudget = 3

    init(core: Core) {
        self.core = core
    }

    var allGranted: Bool {
        accessibility == .granted && microphone == .granted
    }

    /// Nothing has been established yet: the first probe is still out.
    var checking: Bool {
        accessibility == .checking || microphone == .checking
    }

    var accessibilitySupported: Bool {
        accessibility != .unsupported
    }

    func start() async {
        await refresh()
    }

    /// One non-prompting look at both permissions. A `waiting` row stays
    /// waiting until its permission is actually held, so the grant in the other
    /// window is what ends the wait rather than a re-check.
    func refresh() async {
        shellTrusted = AccessibilityTrust.shell
        microphone = Self.settle(microphone, granted: PermissionsMicrophone.state == .granted)
        do {
            let diagnostics: AccessibilityDiagnostics = try await core.request("get_context_diagnostics")
            let wasGranted = accessibility == .granted
            switch diagnostics.accessibility {
            case .granted:
                accessibility = .granted
            case .denied:
                accessibility = Self.settle(accessibility, granted: false)
            case .unsupported:
                accessibility = .unsupported
            }
            // Enigo and the global shortcut listener are started exactly once
            // per grant, and this transition is the only source: the core
            // refuses both while it is untrusted.
            if accessibility == .granted, !wasGranted {
                initializeInput()
            }
            error = nil
            failures = 0
        } catch {
            failures += 1
            self.error = error.localizedDescription
            // A check that cannot run must not leave the flow on a spinner:
            // the row says what it needs and offers its own re-check.
            if accessibility == .checking {
                accessibility = .needed
            }
        }
        if allGranted {
            stopPoll()
            if !announced {
                announced = true
                onAllGranted()
            }
        }
    }

    /// The microphone's own dialog, once. After a denial the system never shows
    /// it again, so the pane takes over rather than a button that does nothing.
    func grantMicrophone() {
        guard PermissionsMicrophone.canPrompt else {
            microphone = .waiting
            openPane(.microphone)
            return
        }
        microphone = .waiting
        startPoll()
        Task {
            _ = await PermissionsMicrophone.request()
            await refresh()
        }
    }

    /// Accessibility has no prompt this process may raise: the system dialog
    /// registers whoever called it, and the caller here is the window, not the
    /// helper that types. So this opens the pane the helper appears in — the
    /// core's own trust check, which the poll runs, is what puts it there.
    func grantAccessibility() {
        accessibility = .waiting
        openPane(.accessibility)
    }

    func openPane(_ pane: PermissionsPane) {
        NSWorkspace.shared.open(pane.url)
        // Settings is open; make sure something is watching for the flip.
        recheck()
    }

    /// Restart the poll with a fresh budget. The budget can stop the poll for
    /// good, which would leave a row waiting on a check that never runs again.
    func recheck() {
        stopPoll()
        failures = 0
        startPoll()
        Task { await refresh() }
    }

    /// Starts the core's typing and shortcut listeners. Idempotent while it
    /// succeeds; a failure lets the next grant try again.
    func initializeInput() {
        guard !inputStarted else { return }
        inputStarted = true
        Task {
            do {
                try await core.request("initialize_enigo")
                try await core.request("initialize_shortcuts")
                error = nil
            } catch {
                inputStarted = false
                self.error = error.localizedDescription
            }
        }
    }

    /// Brings this window forward, the way the web app asked the backend to
    /// reveal itself when a returning user had lost a permission.
    func revealWindow() {
        NSApplication.shared.activate(ignoringOtherApps: true)
    }

    private func startPoll() {
        guard poll == nil else { return }
        poll = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: PermissionsStore.pollInterval)
                guard let self, !Task.isCancelled else { return }
                await self.refresh()
                if self.allGranted || self.failures >= PermissionsStore.failureBudget {
                    self.stopPoll()
                    return
                }
            }
        }
    }

    private func stopPoll() {
        poll?.cancel()
        poll = nil
    }

    /// A row leaves `waiting` only for a grant. Anything else keeps the reader
    /// where they are: they are mid-grant in another window.
    private static func settle(_ current: PermissionState, granted: Bool) -> PermissionState {
        if granted {
            return .granted
        }
        return current == .waiting ? .waiting : .needed
    }
}

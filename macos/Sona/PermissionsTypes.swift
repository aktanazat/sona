import AVFoundation
import AppKit
import ApplicationServices
import Foundation

/// The shapes the first run and the permission surfaces need, and the one fact
/// about this app's layout that decides where a permission has to be checked.
///
/// Sona is two processes. The shell (`com.aktanazat.sona.mac`) draws this
/// window; the core (`com.aktanazat.sona.core`, `Contents/Helpers/SonaCore.app`)
/// records the audio, types the text and owns the global shortcuts. macOS keys
/// Accessibility trust to the process that asks, and `AXIsProcessTrusted`
/// answers only for its own caller — so the shell's own answer is about the
/// shell, which never types. The authoritative answer for the process that does
/// type comes over the socket from `get_context_diagnostics`, which calls
/// `AXIsProcessTrusted()` inside the core (src-tauri/src/context/macos.rs:69).

/// Where one permission stands, in the order a first run moves through it.
/// `waiting` is the state the grant is happening in a window this app does not
/// own: System Settings, or the system's own consent dialog.
enum PermissionState: Equatable {
    case checking
    case needed
    case waiting
    case granted
    /// The platform has no such permission to grant.
    case unsupported
}

/// `AccessibilityAccess` as the core serialises it: one lowercase word each
/// (`#[serde(rename_all = "snake_case")]`, src-tauri/src/context/mod.rs:343).
enum AccessibilityAccessValue: String, Decodable {
    case granted
    case denied
    case unsupported
}

/// `get_context_diagnostics`, narrowed to the field this slice reads. The
/// command is plain (never a Result) and non-prompting, and the value is the
/// core's own Accessibility trust.
struct AccessibilityDiagnostics: Decodable {
    let accessibility: AccessibilityAccessValue
}

/// `get_app_settings`, narrowed to the flag that decides whether the first run
/// flow shows at all. The core sets it to true inside `set_active_model`
/// (src-tauri/src/commands/models.rs:122); there is no command that writes it
/// on its own.
struct OnboardingSettings: Decodable {
    let onboardingCompleted: Bool
}

/// The exact System Settings pane for one permission.
///
/// macOS shows the microphone consent dialog once, ever: after a denial the
/// request resolves silently and a row that only knows how to ask would sit on
/// "Waiting…" with nothing to click. Accessibility is worse — the shell must
/// never raise that prompt, because the prompt registers the *calling* process
/// and the caller here is not the one that types. Deep linking the pane is the
/// way both rows stay actionable.
enum PermissionsPane {
    case accessibility
    case microphone

    var url: URL {
        switch self {
        case .accessibility:
            // Force unwrapped: both are constant, well formed URLs.
            return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")!
        case .microphone:
            return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")!
        }
    }
}

/// What this process is allowed to do with the microphone. This is the shell's
/// own authorization: the only microphone fact a native check can establish,
/// because the core carries its own `NSMicrophoneUsageDescription` and the core
/// exposes no macOS microphone command (`open_microphone_privacy_settings` is
/// Windows only, src-tauri/src/commands/audio.rs:151).
enum PermissionsMicrophone {
    static var state: PermissionState {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return .granted
        case .notDetermined, .denied, .restricted: return .needed
        @unknown default: return .needed
        }
    }

    /// True while the system will still show its consent dialog. After a denial
    /// it never will again, and the pane is the only way back.
    static var canPrompt: Bool {
        AVCaptureDevice.authorizationStatus(for: .audio) == .notDetermined
    }

    static func request() async -> Bool {
        await AVCaptureDevice.requestAccess(for: .audio)
    }
}

/// The shell's own Accessibility trust, for the one sentence that needs it: the
/// name in the Accessibility list is the helper, not this window.
enum AccessibilityTrust {
    /// Non-prompting, and about this process only.
    static var shell: Bool {
        AXIsProcessTrusted()
    }

    /// What the reader has to switch on in System Settings.
    static let coreName = "Sona Core"
}

/// `SecureInputStatus` from `get_secure_input_status` and the
/// `secure-input-changed` event (src-tauri/src/secure_input.rs:26).
struct SecureInputStatus: Decodable {
    let enabled: Bool
    /// On long enough to be stuck, rather than a password field taking focus.
    let sustained: Bool
    let culpritName: String?
    let fallbackActive: Bool
    /// Side specific bindings widened to either side while shadowed.
    let degradedBindings: [String]
    /// Bindings that cannot fire at all.
    let uncoveredBindings: [String]
    /// Someone tried to record a shortcut while secure input was on.
    let recorderBlocked: Bool
}

extension CoreEvent {
    static let secureInputChanged = "secure-input-changed"
    static let onboardingVerificationStarted = "model-verification-started"
    static let onboardingVerificationCompleted = "model-verification-completed"
    static let onboardingExtractionStarted = "model-extraction-started"
    static let onboardingExtractionCompleted = "model-extraction-completed"
}

import SwiftUI

/// macOS Secure Input, while it is stuck on.
///
/// Secure Input — a password field, Terminal's "Secure Keyboard Entry", a wedged
/// loginwindow — stops key events reaching the core's keyboard listener, so
/// keyed shortcuts quietly stop firing. The core's monitor emits
/// `secure-input-changed` on every transition and `sustained` separates the
/// normal momentary activation from a state worth warning about.
@MainActor
@Observable
final class SecureInputStore {
    private(set) var status: SecureInputStatus?
    /// The last thing the core could not do, shown until the next success.
    private(set) var error: String?
    private var dismissed = false

    @ObservationIgnored private let core: Core

    init(core: Core) {
        self.core = core
        core.observe(CoreEvent.secureInputChanged) { [weak self] line in
            guard let self, let status: SecureInputStatus = try? Core.payload(line) else { return }
            self.apply(status)
        }
    }

    func start() async {
        do {
            let status: SecureInputStatus = try await core.request("get_secure_input_status")
            apply(status)
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Warn only where the reader is actually affected: a binding is degraded
    /// (side specific matching widened) or dead (fn+key), or they ran into the
    /// blocked shortcut recorder. When the Carbon fallback covers everything
    /// and nothing else surfaced, stay silent — the core still logs it.
    var impacted: Bool {
        guard let status else { return false }
        if status.recorderBlocked {
            return true
        }
        return status.sustained && !(status.degradedBindings.isEmpty && status.uncoveredBindings.isEmpty)
    }

    var showing: Bool {
        impacted && !dismissed
    }

    /// The one sentence: who is holding the keyboard, and how much of Sona it
    /// costs right now.
    var message: String? {
        guard let status, impacted else { return nil }
        let affected = Set(status.uncoveredBindings).union(status.degradedBindings).count
        guard affected > 0 else {
            if let name = status.culpritName {
                return "\(name) may be blocking shortcut changes"
            }
            return "macOS is temporarily blocking shortcut changes"
        }
        let shortcuts = affected == 1 ? "1 shortcut" : "\(affected) shortcuts"
        if let name = status.culpritName {
            return "\(name) may be blocking \(shortcuts)"
        }
        return "macOS is temporarily blocking \(shortcuts)"
    }

    /// A dismissal lasts for this episode only: once the condition clears, the
    /// next occurrence warns again.
    func dismiss() {
        dismissed = true
    }

    private func apply(_ status: SecureInputStatus) {
        self.status = status
        if !impacted {
            dismissed = false
        }
    }
}

/// The warning itself: one line, and the way to put it away.
struct SecureInputBanner: View {
    let store: SecureInputStore

    var body: some View {
        if store.showing, let message = store.message {
            Card {
                CardRow {
                    HStack(alignment: .firstTextBaseline, spacing: 12) {
                        Image(systemName: "exclamationmark.triangle")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.inkSecondary)
                        Text(message).bodyText(14)
                    }
                } trailing: {
                    Button("Dismiss") { store.dismiss() }
                        .buttonStyle(.quiet)
                }
            }
        }
    }
}

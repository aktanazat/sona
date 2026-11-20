import CallKit
import Foundation
import UserNotifications

/// Watches call state and offers, once per call, to record a note about it.
///
/// It never touches call audio: iOS gives no app access to it, and while a call is live
/// the microphone belongs to the call. `CXCallObserver` needs no permission and reports
/// no number or name — only that a call connected or ended.
@MainActor
final class CallOfferService: NSObject {
    /// Set by the owner; called when the operator taps one of the offers.
    var onOfferAccepted: (() -> Void)?
    /// Set by the owner; called with the conversation when the operator taps an answer
    /// from Sona's agent. This service owns the one notification delegate the app has.
    var onAgentNotification: ((UUID?) -> Void)?

    private let observer = CXCallObserver()
    private var machine = CallOfferMachine()
    private let center = UNUserNotificationCenter.current()
    private static let enabledKey = "sona.callOffers.enabled"
    private static let onCallIdentifier = "sona.call.onCall"
    private static let afterCallIdentifier = "sona.call.afterCall"

    override init() {
        super.init()
        observer.setDelegate(self, queue: nil)
        center.delegate = self
    }

    /// Off until the operator has both accepted the consent sheet and allowed
    /// notifications, then on unless they turn it off.
    var isEnabled: Bool {
        get { UserDefaults.standard.bool(forKey: CallOfferService.enabledKey) }
        set { UserDefaults.standard.set(newValue, forKey: CallOfferService.enabledKey) }
    }

    /// One system prompt, asked on the consent sheet and nowhere else.
    func requestNotifications() async {
        let granted =
            (try? await center.requestAuthorization(options: [.alert, .sound])) ?? false
        if granted { isEnabled = true }
    }

    private func post(_ offer: CallOffer) {
        guard isEnabled else { return }
        let content = UNMutableNotificationContent()
        let identifier: String
        switch offer {
        case .onCall:
            content.title = String(localized: "call.onCall.title")
            content.body = String(localized: "call.onCall.body")
            identifier = CallOfferService.onCallIdentifier
        case let .afterCall(minutes):
            content.title = String(
                format: String(localized: "call.afterCall.title"), minutes
            )
            content.body = String(localized: "call.afterCall.body")
            content.sound = .default
            identifier = CallOfferService.afterCallIdentifier
        }
        center.removePendingNotificationRequests(withIdentifiers: [identifier])
        center.add(
            UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
        )
    }
}

extension CallOfferService: CXCallObserverDelegate {
    /// `CXCallObserver` reports the whole call on every change, so the transition is
    /// read off the call's flags rather than assumed from the callback.
    nonisolated func callObserver(_ callObserver: CXCallObserver, callChanged call: CXCall) {
        let nowUtcMs = Int64(Date().timeIntervalSince1970 * 1000)
        let event: CallEvent? =
            call.hasEnded
            ? .ended(atUtcMs: nowUtcMs)
            : (call.hasConnected ? .connected(atUtcMs: nowUtcMs) : nil)
        guard let event else { return }
        Task { @MainActor [weak self] in
            guard let self, let offer = self.machine.handle(event) else { return }
            self.post(offer)
        }
    }
}

extension CallOfferService: UNUserNotificationCenterDelegate {
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let identifier = response.notification.request.identifier
        let conversation = (response.notification.request.content.userInfo[AgentNotification.conversationKey] as? String)
            .flatMap(UUID.init(uuidString:))
        Task { @MainActor [weak self] in
            if identifier.hasPrefix(AgentNotification.identifierPrefix) {
                self?.onAgentNotification?(conversation)
            } else {
                self?.onOfferAccepted?()
            }
            completionHandler()
        }
    }
}

private extension String {
    init(localized key: String) {
        self = NSLocalizedString(key, comment: "")
    }
}

import Combine
import EventKit
import Foundation

@MainActor
final class PhoneCalendar: ObservableObject {
    @Published private(set) var events: [EKEvent] = []
    @Published private(set) var loading = false
    @Published private(set) var error: String?
    private let store = EKEventStore()
    static let enabledKey = "sona.calendar.enabled"

    func refresh(requestAccess: Bool = false) async {
        guard UserDefaults.standard.bool(forKey: Self.enabledKey) else { events = []; error = nil; return }
        loading = true
        defer { loading = false }
        do {
            if requestAccess, EKEventStore.authorizationStatus(for: .event) == .notDetermined {
                _ = try await store.requestFullAccessToEvents()
            }
            guard EKEventStore.authorizationStatus(for: .event) == .fullAccess else {
                events = []
                error = NSLocalizedString("calendar.denied", comment: "")
                return
            }
            let now = Date()
            let predicate = store.predicateForEvents(withStart: now, end: now.addingTimeInterval(7 * 86400), calendars: nil)
            events = store.events(matching: predicate).filter { !$0.isAllDay && $0.endDate > now }
                .sorted { $0.startDate < $1.startDate }
            error = nil
        } catch { self.error = error.localizedDescription }
    }
}

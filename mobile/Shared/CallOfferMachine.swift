import Foundation

/// What CallKit tells us, reduced to the two transitions that matter.
///
/// `CXCallObserver` reports no number and no name, and iOS gives no app access to call
/// audio, so this is the whole of what the phone can know: a call became live, and a
/// call stopped being live.
enum CallEvent: Equatable {
    case connected(id: UUID, atUtcMs: Int64)
    case ended(id: UUID, atUtcMs: Int64)
}

/// The one thing the phone offers the operator about a call.
enum CallOffer: Equatable {
    /// A call just went live. Sona cannot hear it, and says so.
    case onCall
    /// A call ended after this many whole minutes.
    case afterCall(minutes: Int)
}

/// Call state to at most two offers, with no CallKit and no clock of its own.
///
/// Separate from the observer so the sequence can be tested: CallKit never fires in a
/// simulator, and a state machine driven by real telephony is a state machine nobody
/// can exercise.
struct CallOfferMachine {
    private var activeCalls: Set<UUID> = []
    private var connectedAtUtcMs: Int64 = 0

    mutating func handle(_ event: CallEvent) -> CallOffer? {
        switch event {
        case let .connected(id, atUtcMs):
            let wasIdle = activeCalls.isEmpty
            guard activeCalls.insert(id).inserted, wasIdle else { return nil }
            connectedAtUtcMs = atUtcMs
            return .onCall
        case let .ended(id, atUtcMs):
            guard activeCalls.remove(id) != nil, activeCalls.isEmpty else { return nil }
            return .afterCall(minutes: CallOfferMachine.minutes(from: connectedAtUtcMs, to: atUtcMs))
        }
    }

    var isOnCall: Bool { !activeCalls.isEmpty }

    /// Whole minutes, and never zero: a call the operator remembers taking should not
    /// be reported as having lasted no time at all.
    private static func minutes(from startUtcMs: Int64, to endUtcMs: Int64) -> Int {
        let elapsed = max(0, endUtcMs - startUtcMs)
        return max(1, Int(elapsed / 60_000))
    }
}

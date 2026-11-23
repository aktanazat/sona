import XCTest
import Foundation

/// `CXCallObserver` never fires in a simulator, so the sequence it would produce is
/// driven here by hand. These are the only call transitions the phone can see.
final class CallOfferTests: XCTestCase {
    func testConnectThenEndOffersOnceEach() {
        var machine = CallOfferMachine()
        let call = UUID()
        XCTAssertEqual(machine.handle(.connected(id: call, atUtcMs: 1_000_000)), .onCall)
        XCTAssertTrue(machine.isOnCall)
        XCTAssertEqual(
            machine.handle(.ended(id: call, atUtcMs: 1_000_000 + 245_000)), .afterCall(minutes: 4)
        )
        XCTAssertFalse(machine.isOnCall)
    }

    func testAfterCallWaitsForBothConnectedCallsToEnd() {
        var machine = CallOfferMachine()
        let first = UUID()
        let second = UUID()
        XCTAssertEqual(machine.handle(.connected(id: first, atUtcMs: 0)), .onCall)
        XCTAssertNil(machine.handle(.connected(id: second, atUtcMs: 5_000)))
        XCTAssertNil(machine.handle(.connected(id: second, atUtcMs: 9_000)))
        XCTAssertNil(machine.handle(.ended(id: second, atUtcMs: 60_000)))
        XCTAssertNil(machine.handle(.ended(id: second, atUtcMs: 90_000)))
        XCTAssertEqual(machine.handle(.ended(id: first, atUtcMs: 120_000)), .afterCall(minutes: 2))
    }

    func testMissedCallDoesNotEndAnActiveCall() {
        var machine = CallOfferMachine()
        let connected = UUID()
        XCTAssertEqual(machine.handle(.connected(id: connected, atUtcMs: 0)), .onCall)
        XCTAssertNil(machine.handle(.ended(id: UUID(), atUtcMs: 60_000)))
        XCTAssertEqual(machine.handle(.ended(id: connected, atUtcMs: 120_000)), .afterCall(minutes: 2))
    }

    /// Declined, missed and cancelled calls end without ever connecting; there is
    /// nothing to write a note about.
    func testEndWithoutConnectOffersNothing() {
        var machine = CallOfferMachine()
        XCTAssertNil(machine.handle(.ended(id: UUID(), atUtcMs: 500)))
        XCTAssertFalse(machine.isOnCall)
        XCTAssertNil(machine.handle(.ended(id: UUID(), atUtcMs: 900)))
    }

    func testShortCallStillReportsOneMinute() {
        var machine = CallOfferMachine()
        let call = UUID()
        _ = machine.handle(.connected(id: call, atUtcMs: 0))
        XCTAssertEqual(machine.handle(.ended(id: call, atUtcMs: 4_000)), .afterCall(minutes: 1))
    }

    /// Duration is whole minutes down, so 4 min 59 s is "4 min" and not "5 min".
    func testDurationTruncatesToWholeMinutes() {
        var machine = CallOfferMachine()
        let call = UUID()
        _ = machine.handle(.connected(id: call, atUtcMs: 0))
        XCTAssertEqual(machine.handle(.ended(id: call, atUtcMs: 299_000)), .afterCall(minutes: 4))
    }

    /// A clock that moved backwards between the two callbacks must not produce a
    /// negative or absurd duration.
    func testBackwardsClockDoesNotProduceNonsense() {
        var machine = CallOfferMachine()
        let call = UUID()
        _ = machine.handle(.connected(id: call, atUtcMs: 10_000))
        XCTAssertEqual(machine.handle(.ended(id: call, atUtcMs: 4_000)), .afterCall(minutes: 1))
    }

    func testTwoCallsInARowEachOffer() {
        var machine = CallOfferMachine()
        let first = UUID()
        let second = UUID()
        XCTAssertEqual(machine.handle(.connected(id: first, atUtcMs: 0)), .onCall)
        XCTAssertEqual(machine.handle(.ended(id: first, atUtcMs: 60_000)), .afterCall(minutes: 1))
        XCTAssertEqual(machine.handle(.connected(id: second, atUtcMs: 90_000)), .onCall)
        XCTAssertEqual(machine.handle(.ended(id: second, atUtcMs: 300_000)), .afterCall(minutes: 3))
    }
}

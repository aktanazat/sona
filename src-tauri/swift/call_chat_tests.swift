import XCTest

final class CallChatPolicyTests: XCTestCase {
    private var ready: CallChatEvidence {
        CallChatEvidence(joinedCalls: 1, chatPanes: 1, composers: 1, sendButtons: 1,
                         everyone: true, value: "", insertionAllowed: true, sendAction: true)
    }

    func testAmbiguousComposerRefusesSend() {
        var evidence = ready
        evidence.composers = 2
        XCTAssertNotNil(evidence.refusal)
    }

    func testDraftIncludingWhitespaceRefusesSend() {
        var evidence = ready
        evidence.value = " "
        XCTAssertNotNil(evidence.refusal)
    }

    func testUnreadableDraftRefusesSend() {
        var evidence = ready
        evidence.value = nil
        XCTAssertNotNil(evidence.refusal)
    }

    func testWaitingRoomRefusesSend() {
        var evidence = ready
        evidence.joinedCalls = 0
        XCTAssertNotNil(evidence.refusal)
    }

    func testDirectMessageRefusesSend() {
        var evidence = ready
        evidence.everyone = false
        XCTAssertNotNil(evidence.refusal)
    }

    func testMissingSendActionRefusesSend() {
        var evidence = ready
        evidence.sendAction = false
        XCTAssertNotNil(evidence.refusal)
    }

    func testIdentifiedEmptyPublicChatAllowsAttempt() {
        XCTAssertNil(ready.refusal)
    }
}


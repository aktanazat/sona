import XCTest

final class MeetingLiveTests: XCTestCase {
    /// The countdown the core sends when the app a recording is attributed
    /// to lets go of the microphone. The status is read with `try?`, so a
    /// payload this side cannot decode drops the whole status and hides the
    /// countdown while the core still stops the recording.
    func testAHolderEndingDecodesAndNamesTheApp() throws {
        let payload = #"""
        {"sessionId":"6a2e1864-d2e2-459c-8fbf-81e9ca1312be","trigger":"holder_released",\#
        "stopsAtUtcMs":1763316000000,"appName":"Safari"}
        """#
        let autoStop = try Core.decoder.decode(DetectionAutoStop.self, from: Data(payload.utf8))

        XCTAssertEqual(autoStop.trigger, .holderReleased)
        XCTAssertEqual(autoStop.label, "Safari stopped using the microphone.")
    }

    func testAnEndingWithNoAppStillDecodes() throws {
        let payload = #"""
        {"sessionId":"6a2e1864-d2e2-459c-8fbf-81e9ca1312be","trigger":"event_end",\#
        "stopsAtUtcMs":1763316000000,"appName":null}
        """#
        let autoStop = try Core.decoder.decode(DetectionAutoStop.self, from: Data(payload.utf8))

        XCTAssertEqual(autoStop.label, "Meeting time is up.")
    }
}

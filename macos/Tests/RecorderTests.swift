import XCTest

final class RecorderTests: XCTestCase {
    /// A paused recording holds the microphone, so the core's preflight also
    /// says capture is busy. The sheet opened over it must show the recording
    /// it can still resume, save or discard, not a busy failure.
    func testReopenedSheetShowsThePausedRecordingTheCoreHolds() throws {
        let line = Data("""
        {
          "availability": "supported",
          "startAvailability": "captureBusy",
          "cameraDevices": [],
          "microphoneDevices": [],
          "session": {
            "phase": "paused",
            "elapsedMs": 4000,
            "screenSelected": true,
            "droppedVideoFrames": 0,
            "outputPath": null,
            "width": null,
            "height": null,
            "failure": null
          }
        }
        """.utf8)
        let preflight = try Core.decoder.decode(RecorderPreflight.self, from: line)

        let opened = RecorderUiState()
            .applying(.reset)
            .applying(.checking)
            .applying(.preflight(preflight))

        XCTAssertEqual(opened.snapshot.phase, .paused)
        XCTAssertEqual(opened.snapshot.elapsedMs, 4000)
    }
}

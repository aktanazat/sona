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

    /// Quit waits while a recording opens its file or writes it out, and asks
    /// only once it records. A quit during the save used to go straight on,
    /// and the next launch deleted the half-written file.
    func testQuitWaitsForARecordingThatIsStartingOrSaving() {
        let preview = RecorderSnapshot(
            phase: .previewing, elapsedMs: 0, screenSelected: true, droppedVideoFrames: 0,
            outputPath: nil, width: nil, height: nil, failure: nil)
        var recording = preview
        recording.phase = .recording
        recording.elapsedMs = 4000
        var saved = recording
        saved.phase = .saved
        saved.outputPath = "/Users/someone/Movies/Sona Screen Recording.mp4"

        let previewing = RecorderUiState().applying(.snapshot(preview))
        let starting = previewing.applying(.phase(.starting))
        let recorded = starting.applying(.snapshot(recording))
        let finalizing = recorded.applying(.phase(.finalizing))
        let done = finalizing.applying(.snapshot(saved))

        XCTAssertEqual(
            [previewing, starting, recorded, finalizing, done].map(\.snapshot.phase.activity),
            [.none, .settling, .running, .settling, .none])
    }
}

import XCTest

final class LibraryTests: XCTestCase {
    /// The limit can land while the reader has the field focused. Leaving the
    /// field untouched must not save the number it showed before, which would
    /// delete every unsaved dictation past it.
    func testOnlyATypedLimitIsSaved() {
        var draft = LimitDraft()
        draft.follow(0, editing: false)
        draft.follow(100, editing: true)
        XCTAssertEqual(draft.text, "100", "an untouched field shows the limit that landed")
        XCTAssertNil(draft.commit(ceiling: 1000), "leaving it untouched saves nothing")

        draft.text = "25"
        draft.follow(60, editing: true)
        XCTAssertEqual(draft.text, "25", "a limit landing mid-edit leaves the typing alone")
        XCTAssertEqual(draft.commit(ceiling: 1000), 25)
    }

}
